use super::*;

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn raw_argument_pair_socket_binds_reverse_order_by_parameter_identity() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client, _server) =
            install_raw_client_fixture(&kernel).await?;
        let (active, probe, create_pair, first, second, read_first, read_second) =
            install_raw_argument_pair_socket_fixture(&kernel, &active, &standard_upgrade).await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let denied_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![principal],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&denied_security).await?;

        // The local peer is authenticated. Denial still wins over the reversed
        // same-typed values and their parameter identities.
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let denied_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "argument-pair denied socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: create_pair,
                },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: second,
                    value: RuntimeValue::Text(String::from("denied second")),
                },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: first,
                    value: RuntimeValue::Text(String::from("denied first")),
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 1,
                        failure: CallFailure::ExecuteDenied,
                    },
                "argument-pair denied socket disclosed a target or value fact",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_operation,
            finish_session(shutdown, connection, "argument-pair denied socket cleanup"),
            "argument-pair denied socket operation",
        )?;

        let granted_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_pair),
                ExecuteGrant::new(RAW_CLIENT_USER, read_first),
                ExecuteGrant::new(RAW_CLIENT_USER, read_second),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted_security).await?;
        let reference_credit = u64::try_from(
            encode_active_server_frame(
                &active,
                &ServerFrame::EventBatch {
                    stream: 2,
                    channel: Channel::ResultValues,
                    events: vec![orna_protocol::EventRecord {
                        sequence: 1,
                        event: Event::Value(RuntimeValue::Reference {
                            target: probe,
                            object: ObjectId::from_bytes([0x11; 16]),
                        }),
                    }],
                },
            )?
            .len()
                - 18,
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let granted_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "argument-pair granted socket returned the wrong acknowledgement",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: create_pair,
                },
                ClientFrame::CallArgument {
                    stream: 2,
                    parameter: second,
                    value: RuntimeValue::Text(String::from("stored second")),
                },
                ClientFrame::CallArgument {
                    stream: 2,
                    parameter: first,
                    value: RuntimeValue::Text(String::from("stored first")),
                },
                ClientFrame::CallArgumentsComplete { stream: 2 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 2, .. }
                ),
                "argument-pair granted socket did not accept the complete reverse-order pair",
            )?;
            sleep(Duration::from_millis(50)).await;
            let mut unexpected = [0_u8; 1];
            require(
                matches!(
                    client.try_read(&mut unexpected),
                    Err(error) if error.kind() == ErrorKind::WouldBlock
                ),
                "argument-pair socket emitted a Reference without result-value credit",
            )?;
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: reference_credit,
                },
            )
            .await?;
            let created = read_active_protocol_frame(&mut client, &active).await?;
            let ServerFrame::EventBatch {
                stream: 2, events, ..
            } = created
            else {
                return Err(failure(
                    "argument-pair socket did not emit one Reference event",
                ));
            };
            let [
                orna_protocol::EventRecord {
                    sequence: 1,
                    event: Event::Value(RuntimeValue::Reference { target, object }),
                },
            ] = events.as_slice()
            else {
                return Err(failure(
                    "argument-pair socket returned the wrong create event",
                ));
            };
            require(
                *target == probe && *object != ObjectId::from_bytes([0; 16]),
                "argument-pair socket returned the wrong created Reference",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 2 },
                "argument-pair socket did not complete the credited create",
            )?;

            // The retained zero/one paths, every malformed pair shape, and a
            // pair on a read target stay redacted after accepted framing.
            let mut wrong_bytes = first.to_bytes();
            wrong_bytes[0] ^= 0x01;
            let wrong = ParameterId::from_bytes(wrong_bytes);
            let mut third_bytes = second.to_bytes();
            third_bytes[0] ^= 0x01;
            let third = ParameterId::from_bytes(third_bytes);
            require(
                third != first && third != second && third != wrong,
                "the third pair parameter must be a distinct synthetic identity",
            )?;
            let rejected = [
                (3, create_pair, vec![]),
                (
                    4,
                    create_pair,
                    vec![(first, RuntimeValue::Text(String::from("missing second")))],
                ),
                (
                    5,
                    create_pair,
                    vec![
                        (wrong, RuntimeValue::Text(String::from("wrong"))),
                        (second, RuntimeValue::Text(String::from("second"))),
                    ],
                ),
                (
                    6,
                    create_pair,
                    vec![
                        (first, RuntimeValue::Text(String::from("third first"))),
                        (second, RuntimeValue::Text(String::from("third second"))),
                        (third, RuntimeValue::Text(String::from("third extra"))),
                    ],
                ),
                (
                    7,
                    create_pair,
                    vec![
                        (first, RuntimeValue::Integer(7)),
                        (second, RuntimeValue::Text(String::from("typed second"))),
                    ],
                ),
                (
                    8,
                    read_first,
                    vec![
                        (first, RuntimeValue::Text(String::from("non-insert first"))),
                        (
                            second,
                            RuntimeValue::Text(String::from("non-insert second")),
                        ),
                    ],
                ),
            ];
            for (stream, function, arguments) in rejected {
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallRawStart { stream, function },
                )
                .await?;
                for (parameter, value) in arguments {
                    send_active_protocol_frame(
                        &mut client,
                        &active,
                        &ClientFrame::CallArgument {
                            stream,
                            parameter,
                            value,
                        },
                    )
                    .await?;
                }
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallArgumentsComplete { stream },
                )
                .await?;
                require(
                    matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::CallAccepted { stream: actual, .. } if actual == stream
                    ) && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed {
                            stream,
                            failure: CallFailure::TargetUnavailable,
                        },
                    "argument-pair socket changed a closed target into public detail",
                )?;
            }

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 9,
                    function: create_pair,
                },
                ClientFrame::CallArgument {
                    stream: 9,
                    parameter: second,
                    value: RuntimeValue::Text(String::from("cancel second")),
                },
                ClientFrame::CallArgument {
                    stream: 9,
                    parameter: first,
                    value: RuntimeValue::Text(String::from("cancel first")),
                },
                ClientFrame::CallArgumentsComplete { stream: 9 },
                ClientFrame::CallCancel { stream: 9 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 9, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCancelled { stream: 9 },
                "argument-pair socket did not cancel the accepted complete pair",
            )?;

            for (stream, function, expected) in [
                (
                    10,
                    read_first,
                    [
                        RuntimeValue::Text(String::from("stored first")),
                        RuntimeValue::Text(String::from("cancel first")),
                    ],
                ),
                (
                    11,
                    read_second,
                    [
                        RuntimeValue::Text(String::from("stored second")),
                        RuntimeValue::Text(String::from("cancel second")),
                    ],
                ),
            ] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function },
                    ClientFrame::WindowUpdate {
                        stream,
                        channel: Channel::ResultValues,
                        credit: 1024,
                    },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                let accepted = read_active_protocol_frame(&mut client, &active).await?;
                let first_event = read_active_protocol_frame(&mut client, &active).await?;
                let second_event = read_active_protocol_frame(&mut client, &active).await?;
                let completed = read_active_protocol_frame(&mut client, &active).await?;
                let text_value = |frame: &ServerFrame, sequence| match frame {
                    ServerFrame::EventBatch {
                        stream: actual,
                        events,
                        ..
                    } if *actual == stream
                        && events.len() == 1
                        && events[0].sequence == sequence => match &events[0].event {
                        Event::Value(RuntimeValue::Text(value)) => Some(value.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let first_value = text_value(&first_event, 1);
                let second_value = text_value(&second_event, 2);
                let expected_first = match &expected[0] {
                    RuntimeValue::Text(value) => value.as_str(),
                    _ => unreachable!("the reader oracle uses only Text values"),
                };
                let expected_second = match &expected[1] {
                    RuntimeValue::Text(value) => value.as_str(),
                    _ => unreachable!("the reader oracle uses only Text values"),
                };
                let values_match = (first_value.as_deref() == Some(expected_first)
                    && second_value.as_deref() == Some(expected_second))
                    || (first_value.as_deref() == Some(expected_second)
                        && second_value.as_deref() == Some(expected_first));
                if !matches!(accepted, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream)
                    || !values_match
                    || !matches!(completed, ServerFrame::CallCompleted { stream: actual } if actual == stream)
                {
                    return Err(failure(format!(
                        "argument-pair socket read {stream} returned {accepted:?}, {first_event:?}, {second_event:?}, {completed:?}"
                    )));
                }
            }

            // Duplicate ParameterIds close the protocol connection before a
            // completed RawCall exists. They emit no accepted frame, disclose
            // no target, add no audit, and cannot add a row.
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::CallRawStart {
                    stream: 12,
                    function: create_pair,
                },
            )
            .await?;
            for value in ["duplicate first", "duplicate second"] {
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallArgument {
                        stream: 12,
                        parameter: first,
                        value: RuntimeValue::Text(String::from(value)),
                    },
                )
                .await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await,
                    Err(error) if error.to_string() == "early eof"
                ),
                "a duplicate argument pair did not fail closed before dispatch",
            )?;
            Ok(())
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await?;
        finish_session(
            granted_operation,
            shutdown,
            "argument-pair granted socket operation",
        )?;
        require(
            matches!(connection, Err(LocalRawSocketError::Connection { .. })),
            "a duplicate argument pair did not close the protocol connection",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let actual = audits
            .iter()
            .map(|event| {
                let decision = event.decision();
                (decision.kind(), decision.outcome(), decision.target())
            })
            .collect::<Vec<_>>();
        let allowed = SecurityAuditOutcome::Allowed;
        let expected = vec![
                    (SecurityAuditKind::Authentication, allowed, None),
                    (
                        SecurityAuditKind::Execute,
                        SecurityAuditOutcome::Denied,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (SecurityAuditKind::Authentication, allowed, None),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(read_first, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(create_pair, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(read_first, active.pair())),
                    ),
                    (
                        SecurityAuditKind::Execute,
                        allowed,
                        Some(InvocationTarget::new(read_second, active.pair())),
                    ),
        ];
        if actual != expected {
            return Err(failure(format!(
                "argument-pair socket audit sequence changed: actual {actual:?}; expected {expected:?}"
            )));
        }
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service and ADR 0050 dispatch"]
async fn raw_reference_value_update_socket_retains_pair_authority() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client, _server) =
            install_raw_client_fixture(&kernel).await?;
        let (
            active,
            probe,
            create,
            create_stored,
            update_text,
            text_value,
            text_selector,
            update_link,
            link_value,
            link_selector,
            read_stored,
            read_links,
        ) = install_raw_reference_value_update_socket_fixture(&kernel, &active, &standard_upgrade)
            .await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let denied = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![principal],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&denied).await?;

        // Authentication is local-peer binding. An ungranted pair must not
        // disclose the selected Reference or the private update shape.
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let denied_operation = async {
            client.write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00").await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "reference value denied socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart { stream: 1, function: update_text },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: text_selector,
                    value: RuntimeValue::Reference {
                        target: probe,
                        object: ObjectId::from_bytes([0x31; 16]),
                    },
                },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: text_value,
                    value: RuntimeValue::Text(String::from("denied")),
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 1,
                        failure: CallFailure::ExecuteDenied,
                    },
                "reference value denied socket disclosed an unavailable target fact",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_operation,
            finish_session(shutdown, connection, "reference value denied socket cleanup"),
            "reference value denied socket operation",
        )?;

        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create),
                ExecuteGrant::new(RAW_CLIENT_USER, update_text),
                ExecuteGrant::new(RAW_CLIENT_USER, update_link),
                ExecuteGrant::new(RAW_CLIENT_USER, read_stored),
                ExecuteGrant::new(RAW_CLIENT_USER, read_links),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let direct_session = granted.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let reference_credit = u64::try_from(
            encode_active_server_frame(
                &active,
                &ServerFrame::EventBatch {
                    stream: 3,
                    channel: Channel::ResultValues,
                    events: vec![orna_protocol::EventRecord {
                        sequence: 1,
                        event: Event::Value(RuntimeValue::Reference {
                            target: probe,
                            object: ObjectId::from_bytes([0x32; 16]),
                        }),
                    }],
                },
            )?
            .len()
                - 18,
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let granted_operation = async {
            client.write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00").await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "reference value granted socket returned the wrong acknowledgement",
            )?;

            let mut created = Vec::new();
            for (stream, stored) in [(1, "first"), (2, "second")] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function: create },
                    ClientFrame::CallArgument {
                        stream,
                        parameter: create_stored,
                        value: RuntimeValue::Text(String::from(stored)),
                    },
                    ClientFrame::WindowUpdate {
                        stream,
                        channel: Channel::ResultValues,
                        credit: 1024,
                    },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream),
                    "reference value socket did not accept the create call",
                )?;
                let event = read_active_protocol_frame(&mut client, &active).await?;
                let ServerFrame::EventBatch { events, .. } = event else {
                    return Err(failure("reference value socket create did not return an event batch"));
                };
                let [orna_protocol::EventRecord {
                    event: Event::Value(RuntimeValue::Reference { target, object }), ..
                }] = events.as_slice() else {
                    return Err(failure("reference value socket create did not return one Reference"));
                };
                require(
                    *target == probe && *object != ObjectId::from_bytes([0; 16]),
                    "reference value socket create returned the wrong Reference",
                )?;
                created.push(RuntimeValue::Reference { target: *target, object: *object });
                require(
                    read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream },
                    "reference value socket create did not complete",
                )?;
            }
            let [first, second] = created.as_slice() else {
                return Err(failure("reference value socket did not create two rows"));
            };

            // The raw socket closes both conflict forms to the same public
            // failure. The trusted direct dispatcher retains the typed
            // source, because no socket frame may expose it.
            let duplicate_value = RuntimeValue::Text(String::from("second"));
            let duplicate_insert = RawClientDispatch::new(
                kernel.clone(),
                direct_session.clone(),
                12,
                RawCall {
                    function: create,
                    arguments: vec![orna_protocol::CallArgument {
                        parameter: create_stored,
                        value: duplicate_value.clone(),
                    }],
                },
            )
            .finish()
            .await;
            require_dispatch_failure(
                &duplicate_insert,
                12,
                CallFailure::InternalFailure,
                matches!(
                    duplicate_insert.source(),
                    Some(PostgresKernelError::ServerInsert(
                        ServerInsertError::NotCommitted { source, .. }
                    )) if matches!(source.as_ref(), ServerMutationError::UniqueTextConflict { .. })
                ),
                "a duplicate raw Text INSERT did not retain its private typed conflict",
            )?;
            let duplicate_update = RawClientDispatch::new(
                kernel.clone(),
                direct_session.clone(),
                13,
                RawCall {
                    function: update_text,
                    arguments: vec![
                        orna_protocol::CallArgument {
                            parameter: text_selector,
                            value: first.clone(),
                        },
                        orna_protocol::CallArgument {
                            parameter: text_value,
                            value: duplicate_value.clone(),
                        },
                    ],
                },
            )
            .finish()
            .await;
            require_dispatch_failure(
                &duplicate_update,
                13,
                CallFailure::InternalFailure,
                matches!(
                    duplicate_update.source(),
                    Some(PostgresKernelError::ServerUpdate(
                        ServerUpdateError::NotCommitted { source, .. }
                    )) if matches!(source.as_ref(), ServerMutationError::UniqueTextConflict { .. })
                ),
                "a duplicate raw Text UPDATE did not retain its private typed conflict",
            )?;

            for (stream, function, arguments) in [
                (
                    3,
                    create,
                    vec![(create_stored, duplicate_value.clone())],
                ),
                (
                    4,
                    update_text,
                    vec![
                        (text_selector, first.clone()),
                        (text_value, duplicate_value.clone()),
                    ],
                ),
            ] {
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallRawStart { stream, function },
                )
                .await?;
                for (parameter, value) in arguments {
                    send_active_protocol_frame(
                        &mut client,
                        &active,
                        &ClientFrame::CallArgument {
                            stream,
                            parameter,
                            value,
                        },
                    )
                    .await?;
                }
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallArgumentsComplete { stream },
                )
                .await?;
                require(
                    matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::CallAccepted { stream: actual, .. } if actual == stream
                    ) && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed {
                            stream,
                            failure: CallFailure::InternalFailure,
                        },
                    "a duplicate raw Text mutation disclosed a private conflict fact",
                )?;
                require(
                    timeout(
                        Duration::from_millis(50),
                        read_active_protocol_frame(&mut client, &active),
                    )
                    .await
                    .is_err(),
                    "a duplicate raw Text mutation emitted a value frame after its terminal failure",
                )?;
            }

            // Selector-first framing reverses the declaration order. The
            // accepted call emits no value until exact result credit arrives.
            for frame in [
                ClientFrame::CallRawStart { stream: 5, function: update_text },
                ClientFrame::CallArgument { stream: 5, parameter: text_selector, value: first.clone() },
                ClientFrame::CallArgument {
                    stream: 5,
                    parameter: text_value,
                    value: RuntimeValue::Text(String::from("changed")),
                },
                ClientFrame::CallArgumentsComplete { stream: 5 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 5, .. }),
                "reference value socket did not accept the scalar pair UPDATE",
            )?;
            sleep(Duration::from_millis(50)).await;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active)
                )
                .await
                .is_err(),
                "reference value socket emitted an UPDATE result without credit",
            )?;
            send_active_protocol_frame(&mut client, &active, &ClientFrame::WindowUpdate {
                stream: 5,
                channel: Channel::ResultValues,
                credit: reference_credit
                    .checked_sub(1)
                    .ok_or_else(|| failure("reference value event credit must be nonzero"))?,
            })
            .await?;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active)
                )
                .await
                .is_err(),
                "reference value socket emitted an UPDATE result before its exact credit boundary",
            )?;
            send_active_protocol_frame(&mut client, &active, &ClientFrame::WindowUpdate {
                stream: 5, channel: Channel::ResultValues, credit: 1,
            }).await?;
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::EventBatch { stream: 5, events, .. }
                        if events.len() == 1 && events[0].sequence == 1
                            && events[0].event == Event::Value(first.clone())
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 5 },
                "reference value socket scalar UPDATE did not return the exact selector",
            )?;

            let RuntimeValue::Reference { object: first_object, .. } = first else {
                return Err(failure("first reference value socket row is not a Reference"));
            };
            let RuntimeValue::Reference { object: second_object, .. } = second else {
                return Err(failure("second reference value socket row is not a Reference"));
            };
            let absent_object = [[0x41; 16], [0x42; 16], [0x43; 16]]
                .into_iter()
                .map(ObjectId::from_bytes)
                .find(|candidate| candidate != first_object && candidate != second_object)
                .ok_or_else(|| failure("reference value socket has no absent object identity"))?;
            for frame in [
                ClientFrame::CallRawStart { stream: 6, function: update_text },
                ClientFrame::CallArgument { stream: 6, parameter: text_value, value: RuntimeValue::Text(String::from("absent")) },
                ClientFrame::CallArgument {
                    stream: 6,
                    parameter: text_selector,
                    value: RuntimeValue::Reference { target: probe, object: absent_object },
                },
                ClientFrame::WindowUpdate { stream: 6, channel: Channel::ResultValues, credit: 1024 },
                ClientFrame::CallArgumentsComplete { stream: 6 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 6, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream: 6 },
                "reference value socket absent selector did not complete empty",
            )?;

            for frame in [
                ClientFrame::CallRawStart { stream: 7, function: update_link },
                ClientFrame::CallArgument { stream: 7, parameter: link_selector, value: second.clone() },
                ClientFrame::CallArgument { stream: 7, parameter: link_value, value: first.clone() },
                ClientFrame::WindowUpdate { stream: 7, channel: Channel::ResultValues, credit: 1024 },
                ClientFrame::CallArgumentsComplete { stream: 7 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 7, .. })
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 7, events, .. }
                            if events.len() == 1 && events[0].event == Event::Value(second.clone())
                    ) && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream: 7 },
                "reference value socket Reference UPDATE did not bind the selected row",
            )?;

            // Cancellation remains public. The accepted mutation commits, so
            // the later socket read is the durable oracle.
            for frame in [
                ClientFrame::CallRawStart { stream: 8, function: update_text },
                ClientFrame::CallArgument { stream: 8, parameter: text_selector, value: first.clone() },
                ClientFrame::CallArgument { stream: 8, parameter: text_value, value: RuntimeValue::Text(String::from("second\n")) },
                ClientFrame::CallArgumentsComplete { stream: 8 },
                ClientFrame::CallCancel { stream: 8 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 8, .. })
                    && read_active_protocol_frame(&mut client, &active).await? == ServerFrame::CallCancelled { stream: 8 },
                "reference value socket did not retain accepted-call cancellation",
            )?;

            let mut wrong_bytes = text_selector.to_bytes();
            wrong_bytes[0] ^= 1;
            let wrong = ParameterId::from_bytes(wrong_bytes);
            for frame in [
                ClientFrame::CallRawStart { stream: 9, function: update_text },
                ClientFrame::CallArgument { stream: 9, parameter: wrong, value: first.clone() },
                ClientFrame::CallArgument { stream: 9, parameter: text_value, value: RuntimeValue::Text(String::from("wrong")) },
                ClientFrame::CallArgumentsComplete { stream: 9 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 9, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed { stream: 9, failure: CallFailure::TargetUnavailable },
                "reference value socket invalid pair did not stay redacted",
            )?;

            for (stream, function, arguments) in [
                (
                    10,
                    update_text,
                    vec![
                        (text_selector, RuntimeValue::Text(String::from("not-a-reference"))),
                        (text_value, first.clone()),
                    ],
                ),
                (
                    11,
                    read_stored,
                    vec![
                        (text_selector, first.clone()),
                        (text_value, RuntimeValue::Text(String::from("not-an-update"))),
                    ],
                ),
            ] {
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallRawStart { stream, function },
                )
                .await?;
                for (parameter, value) in arguments {
                    send_active_protocol_frame(
                        &mut client,
                        &active,
                        &ClientFrame::CallArgument {
                            stream,
                            parameter,
                            value,
                        },
                    )
                    .await?;
                }
                send_active_protocol_frame(
                    &mut client,
                    &active,
                    &ClientFrame::CallArgumentsComplete { stream },
                )
                .await?;
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream)
                        && matches!(
                            read_active_protocol_frame(&mut client, &active).await?,
                            ServerFrame::CallFailed { stream: actual, failure: CallFailure::TargetUnavailable }
                                if actual == stream
                        ),
                    "reference value socket mistyped or non-update pair disclosed a target fact",
                )?;
            }

            for (stream, function) in [(12, read_stored), (13, read_links)] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function },
                    ClientFrame::WindowUpdate { stream, channel: Channel::ResultValues, credit: 2048 },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream),
                    "reference value socket did not accept its durable reader",
                )?;
                let first_event = read_active_protocol_frame(&mut client, &active).await?;
                let second_event = read_active_protocol_frame(&mut client, &active).await?;
                let completed = read_active_protocol_frame(&mut client, &active).await?;
                let values = [first_event, second_event].into_iter().filter_map(|frame| match frame {
                    ServerFrame::EventBatch { events, .. } if events.len() == 1 => match &events[0].event {
                        Event::Value(value) => Some(value.clone()),
                        _ => None,
                    },
                    _ => None,
                }).collect::<Vec<_>>();
                if !matches!(completed, ServerFrame::CallCompleted { stream: actual } if actual == stream) {
                    return Err(failure("reference value socket durable reader did not complete"));
                }
                if stream == 12 {
                    require(
                        values.iter().filter(|value| **value == RuntimeValue::Text(String::from("second\n"))).count() == 1
                            && values.iter().filter(|value| **value == RuntimeValue::Text(String::from("second"))).count() == 1,
                        "reference value socket did not preserve a byte-distinct Text value after cancellation",
                    )?;
                } else {
                    require(
                        values.iter().filter(|value| **value == *first).count() == 1,
                        "reference value socket Reference UPDATE did not store its Reference value",
                    )?;
                }
            }
            Ok(())
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            granted_operation,
            finish_session(shutdown, connection, "reference value granted socket cleanup"),
            "reference value granted socket operation",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let expected = [
            (SecurityAuditKind::Authentication, SecurityAuditOutcome::Allowed, None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Denied, Some(update_text)),
            (SecurityAuditKind::Authentication, SecurityAuditOutcome::Allowed, None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_link)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(update_text)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(read_stored)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(read_stored)),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(read_links)),
        ];
        require(
            audits.len() == expected.len()
                && audits.iter().zip(expected).all(|(event, (kind, outcome, function))| {
                    let decision = event.decision();
                    decision.kind() == kind
                        && decision.outcome() == outcome
                        && decision.session_principal() == Some(RAW_CLIENT_USER)
                        && decision.target().map(InvocationTarget::function) == function
                }),
            "reference value socket changed the private typed audit sequence",
        )?;
        let audit_debug = format!("{audits:?}");
        require(
            !audit_debug.contains("second") && !audit_debug.contains("second\\n"),
            "unique Text values leaked into the durable security audit",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

/// Proves the ADR 0052 version-4 unique Text read through the authenticated
/// public raw socket. The direct dispatcher retains the private duplicate
/// conflict source. The socket must expose only its public failure frame.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service and ADR 0052 dispatch"]
async fn raw_unique_text_select_socket_authorises_binds_and_redacts() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client, _server) =
            install_raw_client_fixture(&kernel).await?;
        let (active, person, create, create_email, create_name, by_email, email, all_people) =
            install_raw_unique_text_select_socket_fixture(&kernel, &active, &standard_upgrade)
                .await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let denied = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![principal],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&denied).await?;
        let mut wrong_email_bytes = email.to_bytes();
        wrong_email_bytes[0] ^= 1;
        let wrong_email = ParameterId::from_bytes(wrong_email_bytes);

        // A denied malformed call must not disclose the parameter or value
        // error that would otherwise make its target unavailable.
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let denied_operation = async {
            client.write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00").await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "unique Text denied socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart { stream: 1, function: by_email },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: wrong_email,
                    value: RuntimeValue::Integer(42),
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 1, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed { stream: 1, failure: CallFailure::ExecuteDenied },
                "unique Text denied socket disclosed a target or value fact",
            )?;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active),
                )
                .await
                .is_err(),
                "unique Text denied socket emitted a value after ExecuteDenied",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_operation,
            finish_session(shutdown, connection, "unique Text denied socket cleanup"),
            "unique Text denied socket operation",
        )?;

        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create),
                ExecuteGrant::new(RAW_CLIENT_USER, by_email),
                ExecuteGrant::new(RAW_CLIENT_USER, all_people),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let direct = granted.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let reference_credit = u64::try_from(
            encode_active_server_frame(
                &active,
                &ServerFrame::EventBatch {
                    stream: 4,
                    channel: Channel::ResultValues,
                    events: vec![orna_protocol::EventRecord {
                        sequence: 1,
                        event: Event::Value(RuntimeValue::Reference {
                            target: person,
                            object: ObjectId::from_bytes([0x41; 16]),
                        }),
                    }],
                },
            )?
            .len()
                - 18,
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let granted_operation = async {
            client.write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00").await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "unique Text granted socket returned the wrong acknowledgement",
            )?;

            let mut created = Vec::new();
            for (stream, value, name) in [
                (1, "caf\u{e9}", "exact bytes"),
                (2, "cafe\u{301}", "byte-distinct decomposed"),
            ] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function: create },
                    ClientFrame::CallArgument {
                        stream,
                        parameter: create_email,
                        value: RuntimeValue::Text(value.into()),
                    },
                    ClientFrame::CallArgument {
                        stream,
                        parameter: create_name,
                        value: RuntimeValue::Text(name.into()),
                    },
                    ClientFrame::WindowUpdate {
                        stream,
                        channel: Channel::ResultValues,
                        credit: 1024,
                    },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream),
                    "unique Text socket did not accept row creation",
                )?;
                let ServerFrame::EventBatch { events, .. } =
                    read_active_protocol_frame(&mut client, &active).await?
                else {
                    return Err(failure("unique Text socket creator did not return an event batch"));
                };
                let [orna_protocol::EventRecord {
                    event: Event::Value(RuntimeValue::Reference { target, object }), ..
                }] = events.as_slice() else {
                    return Err(failure("unique Text socket creator did not return one Reference"));
                };
                require(
                    *target == person && *object != ObjectId::from_bytes([0; 16]),
                    "unique Text socket creator returned the wrong Reference",
                )?;
                created.push(RuntimeValue::Reference { target: *target, object: *object });
                require(
                    read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream },
                    "unique Text socket creator did not complete",
                )?;
            }
            let [exact_reference, decomposed_reference] = created.as_slice() else {
                return Err(failure("unique Text socket did not create both byte-distinct rows"));
            };

            // The private cause remains available to the direct dispatcher.
            // The same duplicate through the socket may expose only ORF1
            // `InternalFailure`, with no following value frame.
            let duplicate = RawClientDispatch::new(
                kernel.clone(),
                direct,
                30,
                RawCall {
                    function: create,
                    arguments: vec![
                        orna_protocol::CallArgument {
                            parameter: create_email,
                            value: RuntimeValue::Text("caf\u{e9}".into()),
                        },
                        orna_protocol::CallArgument {
                            parameter: create_name,
                            value: RuntimeValue::Text("duplicate private source".into()),
                        },
                    ],
                },
            )
            .finish()
            .await;
            require_dispatch_failure(
                &duplicate,
                30,
                CallFailure::InternalFailure,
                matches!(
                    duplicate.source(),
                    Some(PostgresKernelError::ServerInsert(
                        ServerInsertError::NotCommitted { source, .. }
                    )) if matches!(source.as_ref(), ServerMutationError::UniqueTextConflict { .. })
                ),
                "unique Text duplicate did not retain its private typed conflict",
            )?;
            for frame in [
                ClientFrame::CallRawStart { stream: 3, function: create },
                ClientFrame::CallArgument {
                    stream: 3,
                    parameter: create_email,
                    value: RuntimeValue::Text("caf\u{e9}".into()),
                },
                ClientFrame::CallArgument {
                    stream: 3,
                    parameter: create_name,
                    value: RuntimeValue::Text("duplicate socket source".into()),
                },
                ClientFrame::CallArgumentsComplete { stream: 3 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 3, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed { stream: 3, failure: CallFailure::InternalFailure },
                "unique Text socket disclosed its private duplicate conflict",
            )?;
            require(
                timeout(Duration::from_millis(50), read_active_protocol_frame(&mut client, &active))
                    .await
                    .is_err(),
                "unique Text socket emitted a value after InternalFailure",
            )?;

            // Give exact reference-frame credit. The version-4 selector must
            // stop at the next projection until more credit is supplied.
            for frame in [
                ClientFrame::CallRawStart { stream: 4, function: by_email },
                ClientFrame::CallArgument {
                    stream: 4,
                    parameter: email,
                    value: RuntimeValue::Text("caf\u{e9}".into()),
                },
                ClientFrame::WindowUpdate {
                    stream: 4,
                    channel: Channel::ResultValues,
                    credit: reference_credit,
                },
                ClientFrame::CallArgumentsComplete { stream: 4 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 4, .. })
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 4, events, .. }
                            if events.len() == 1 && events[0].sequence == 1
                                && events[0].event == Event::Value(exact_reference.clone())
                    ),
                "unique Text socket did not return its first exact ORF1 value",
            )?;
            sleep(Duration::from_millis(50)).await;
            let mut unexpected = [0_u8; 1];
            require(
                matches!(client.try_read(&mut unexpected), Err(error) if error.kind() == ErrorKind::WouldBlock),
                "unique Text socket emitted a projection before its credit boundary",
            )?;
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::WindowUpdate {
                    stream: 4,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
            )
            .await?;
            let expected_null = Event::Value(RuntimeValue::null(ResolvedType::scalar(
                orna_core::types::StandardScalar::CharacterLargeObject,
            ))?);
            for (sequence, expected) in [
                (2, Event::Value(RuntimeValue::Text("exact bytes".into()))),
                (3, expected_null),
            ] {
                let actual = read_active_protocol_frame(&mut client, &active).await?;
                if !matches!(
                    actual,
                    ServerFrame::EventBatch { stream: 4, ref events, .. }
                        if events.len() == 1 && events[0].sequence == sequence
                            && events[0].event == expected
                ) {
                    return Err(failure(format!(
                        "unique Text socket did not preserve exact ordered ORF1 values: {actual:?}"
                    )));
                }
            }
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 4 },
                "unique Text socket did not complete its exact row",
            )?;

            for frame in [
                ClientFrame::CallRawStart { stream: 5, function: by_email },
                ClientFrame::CallArgument {
                    stream: 5,
                    parameter: email,
                    value: RuntimeValue::Text("cafe\u{301}".into()),
                },
                ClientFrame::WindowUpdate {
                    stream: 5,
                    channel: Channel::ResultValues,
                    credit: 2048,
                },
                ClientFrame::CallArgumentsComplete { stream: 5 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            let expected_decomposed_null = Event::Value(RuntimeValue::null(ResolvedType::scalar(
                orna_core::types::StandardScalar::CharacterLargeObject,
            ))?);
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 5, .. })
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 5, events, .. }
                            if events.len() == 1 && events[0].sequence == 1
                                && events[0].event == Event::Value(decomposed_reference.clone())
                    )
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 5, events, .. }
                            if events.len() == 1 && events[0].sequence == 2
                                && events[0].event == Event::Value(RuntimeValue::Text("byte-distinct decomposed".into()))
                    )
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 5, events, .. }
                            if events.len() == 1 && events[0].sequence == 3
                                && events[0].event == expected_decomposed_null
                    )
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream: 5 },
                "unique Text socket did not select the C-byte-distinct row",
            )?;

            for frame in [
                ClientFrame::CallRawStart { stream: 6, function: by_email },
                ClientFrame::CallArgument {
                    stream: 6,
                    parameter: email,
                    value: RuntimeValue::Text("absent@example.test".into()),
                },
                ClientFrame::CallArgumentsComplete { stream: 6 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 6, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallCompleted { stream: 6 },
                "unique Text socket did not complete an absent value without output",
            )?;

            for (stream, function, parameter, value) in [
                (7, by_email, wrong_email, RuntimeValue::Text("caf\u{e9}".into())),
                (8, by_email, email, RuntimeValue::Integer(42)),
                (9, all_people, email, RuntimeValue::Text("caf\u{e9}".into())),
            ] {
                for frame in [
                    ClientFrame::CallRawStart { stream, function },
                    ClientFrame::CallArgument { stream, parameter, value },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream)
                        && read_active_protocol_frame(&mut client, &active).await?
                            == ServerFrame::CallFailed { stream, failure: CallFailure::TargetUnavailable },
                    "unique Text socket disclosed a closed target fact",
                )?;
                require(
                    timeout(
                        Duration::from_millis(50),
                        read_active_protocol_frame(&mut client, &active),
                    )
                    .await
                    .is_err(),
                    "unique Text socket emitted a value after TargetUnavailable",
                )?;
            }

            let typed_null = RuntimeValue::null(ResolvedType::scalar(
                orna_core::types::StandardScalar::CharacterLargeObject,
            ))?;
            for frame in [
                ClientFrame::CallRawStart { stream: 10, function: by_email },
                ClientFrame::CallArgument {
                    stream: 10,
                    parameter: email,
                    value: typed_null,
                },
                ClientFrame::CallArgumentsComplete { stream: 10 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 10, .. })
                    && read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed { stream: 10, failure: CallFailure::TargetUnavailable },
                "unique Text socket did not reject a typed NULL selector",
            )?;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active),
                )
                .await
                .is_err(),
                "unique Text socket emitted a value after its NULL closure",
            )?;

            // PostgreSQL C equality must not fold case, trim whitespace, or
            // change line endings. None of these byte-distinct values exists.
            for (stream, value) in [
                (11, "CAF\u{c9}"),
                (12, "caf\u{e9} "),
                (13, "caf\u{e9}\n"),
                (14, "caf\u{e9}\r\n"),
            ] {
                for frame in [
                    ClientFrame::CallRawStart {
                        stream,
                        function: by_email,
                    },
                    ClientFrame::CallArgument {
                        stream,
                        parameter: email,
                        value: RuntimeValue::Text(value.into()),
                    },
                    ClientFrame::CallArgumentsComplete { stream },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: actual, .. } if actual == stream)
                        && read_active_protocol_frame(&mut client, &active).await?
                            == ServerFrame::CallCompleted { stream },
                    "unique Text socket folded a byte-distinct selector",
                )?;
            }

            // The call must remain cancellable after its first selected value
            // has crossed the public socket. Withheld credit keeps it open.
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 15,
                    function: by_email,
                },
                ClientFrame::CallArgument {
                    stream: 15,
                    parameter: email,
                    value: RuntimeValue::Text("caf\u{e9}".into()),
                },
                ClientFrame::WindowUpdate {
                    stream: 15,
                    channel: Channel::ResultValues,
                    credit: reference_credit,
                },
                ClientFrame::CallArgumentsComplete { stream: 15 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(read_active_protocol_frame(&mut client, &active).await?, ServerFrame::CallAccepted { stream: 15, .. })
                    && matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::EventBatch { stream: 15, events, .. }
                            if events.len() == 1 && events[0].sequence == 1
                                && events[0].event == Event::Value(exact_reference.clone())
                    ),
                "unique Text socket did not begin the cancellable version-4 result",
            )?;
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::CallCancel { stream: 15 },
            )
            .await?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCancelled { stream: 15 },
                "unique Text socket did not cancel after its first result value",
            )?;
            require(
                timeout(
                    Duration::from_millis(50),
                    read_active_protocol_frame(&mut client, &active),
                )
                .await
                .is_err(),
                "unique Text socket emitted a frame after cancellation",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            granted_operation,
            finish_session(shutdown, connection, "unique Text granted socket cleanup"),
            "unique Text granted socket operation",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let expected = [
            (SecurityAuditKind::Authentication, SecurityAuditOutcome::Allowed, None, None),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Denied,
                Some(by_email),
                Some(SecurityAuditDenial::Execute(ExecuteDenial::MissingExecuteGrant)),
            ),
            (SecurityAuditKind::Authentication, SecurityAuditOutcome::Allowed, None, None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(create), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(all_people), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
            (SecurityAuditKind::Execute, SecurityAuditOutcome::Allowed, Some(by_email), None),
        ];
        let audit_matches = audits.len() == expected.len()
            && audits.iter().zip(expected).all(|(event, (kind, outcome, function, denial))| {
                let decision = event.decision();
                decision.kind() == kind
                    && decision.outcome() == outcome
                    && decision.session_principal() == Some(RAW_CLIENT_USER)
                    && decision.target()
                        == function.map(|function| InvocationTarget::new(function, active.pair()))
                    && decision.denial() == denial
            });
        if !audit_matches {
            return Err(failure(format!(
                "unique Text socket changed its ordered durable audit decisions: {audits:?}"
            )));
        }
        let audit_debug = format!("{audits:?}");
        require(
            !audit_debug.contains("caf\u{e9}")
                && !audit_debug.contains("cafe\u{301}")
                && !audit_debug.contains("exact bytes")
                && !audit_debug.contains("byte-distinct decomposed")
                && !audit_debug.contains("duplicate private source")
                && !audit_debug.contains("duplicate socket source")
                && !audit_debug.contains("absent@example.test")
                && !audit_debug.contains("CAF\u{c9}")
                && !audit_debug.contains("caf\u{e9} ")
                && !audit_debug.contains("caf\u{e9}\n")
                && !audit_debug.contains("caf\u{e9}\r\n"),
            "unique Text selector values leaked into the durable security audit",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn serves_the_actual_local_peer_through_the_raw_socket_protocol() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("raw-local-peer-live".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| failure(format!("raw local peer runtime failed: {error}")))?;
            runtime.block_on(serves_the_actual_local_peer_through_the_raw_socket_protocol_inner())
        })
        .map_err(|error| failure(format!("raw local peer thread could not start: {error}")))?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("raw local peer thread panicked")),
    }
}

async fn serves_the_actual_local_peer_through_the_raw_socket_protocol_inner() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        insert_raw_server_flag(&database, &active, 0x7f, true).await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client_function),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let mut server_value_wires = Vec::new();
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let version_two_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x02\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x02\x00\x00\x00\x00",
                "local raw socket returned the wrong catalogue acknowledgement",
            )?;

            send_catalogue_protocol_frame(
                &mut client,
                active.catalogue(),
                &ClientFrame::CallRawStart {
                    stream: 1,
                    function: client_function,
                },
            )
            .await?;
            send_catalogue_protocol_frame(
                &mut client,
                active.catalogue(),
                &ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
            )
            .await?;
            send_catalogue_protocol_frame(
                &mut client,
                active.catalogue(),
                &ClientFrame::CallArgumentsComplete { stream: 1 },
            )
            .await?;
            require(
                matches!(
                    read_catalogue_protocol_frame(&mut client, active.catalogue()).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "local raw socket did not accept the catalogue CLIENT call",
            )?;
            require(
                matches!(
                    read_catalogue_protocol_frame(&mut client, active.catalogue()).await?,
                    ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "local raw socket returned the wrong catalogue CLIENT value",
            )?;
            require(
                read_catalogue_protocol_frame(&mut client, active.catalogue()).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "local raw socket did not complete the catalogue CLIENT call",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
                ClientFrame::CallArgumentsComplete { stream: 2 },
            ] {
                send_catalogue_protocol_frame(&mut client, active.catalogue(), &frame).await?;
            }
            require(
                matches!(
                    read_catalogue_protocol_frame(&mut client, active.catalogue()).await?,
                    ServerFrame::CallAccepted { stream: 2, .. }
                ),
                "protocol-2 socket did not accept the raw SERVER call",
            )?;
            let encoded = read_encoded_protocol_frame(&mut client).await?;
            require(
                matches!(
                    orna_protocol::decode_catalogue_server_frame(active.catalogue(), &encoded)?,
                    ServerFrame::EventBatch {
                        stream: 2,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-2 socket returned the wrong raw SERVER value",
            )?;
            server_value_wires.push(canonical_value_suffix(&encoded, b"ORV2")?);
            require(
                read_catalogue_protocol_frame(&mut client, active.catalogue()).await?
                    == ServerFrame::CallCompleted { stream: 2 },
                "protocol-2 socket did not complete the raw SERVER call",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let cleanup = finish_session(shutdown, connection, "local raw socket connection cleanup");
        finish_session(
            version_two_operation,
            cleanup,
            "local raw socket protocol-2 operation",
        )?;

        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let version_one_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00",
                "local raw socket returned the wrong protocol-1 acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_legacy_protocol_frame(&mut client, &frame).await?;
            }
            require(
                matches!(
                    read_legacy_protocol_frame(&mut client).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "protocol-1 socket did not accept the raw SERVER call",
            )?;
            let encoded = read_encoded_protocol_frame(&mut client).await?;
            require(
                matches!(
                    decode_server_frame(&encoded)?,
                    ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-1 socket returned the wrong raw SERVER value",
            )?;
            server_value_wires.push(canonical_value_suffix(&encoded, b"ORV1")?);
            require(
                read_legacy_protocol_frame(&mut client).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "protocol-1 socket did not complete the raw SERVER call",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            version_one_operation,
            finish_session(
                shutdown,
                connection,
                "protocol-1 raw socket connection cleanup",
            ),
            "local raw socket protocol-1 operation",
        )?;

        let record = raw_client_record(&active)?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let mut current_pair = active.pair();
        let version_three_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "local raw socket returned the wrong active-revision acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "protocol-3 socket did not accept the raw SERVER call",
            )?;
            let encoded = read_encoded_protocol_frame(&mut client).await?;
            require(
                matches!(
                    decode_active_server_frame(&active, &encoded)?,
                    ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-3 socket returned the wrong raw SERVER value",
            )?;
            server_value_wires.push(canonical_value_suffix(&encoded, b"ORV3")?);
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "protocol-3 socket did not complete the raw SERVER call",
            )?;

            insert_raw_server_flag(&database, &active, 0x80, true).await?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: BOOLEAN_EVENT_CREDIT,
                },
                ClientFrame::CallArgumentsComplete { stream: 2 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 2, .. }
                ),
                "protocol-3 socket did not accept the flow-controlled multi-row SERVER call",
            )?;
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::EventBatch {
                        stream: 2,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 1
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-3 socket did not emit the first ordered SERVER row under exact credit",
            )?;
            sleep(Duration::from_millis(50)).await;
            let mut unexpected = [0_u8; 1];
            require(
                matches!(
                    client.try_read(&mut unexpected),
                    Err(error) if error.kind() == ErrorKind::WouldBlock
                ),
                "protocol-3 socket emitted a second SERVER row without result-value credit",
            )?;
            send_active_protocol_frame(
                &mut client,
                &active,
                &ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: BOOLEAN_EVENT_CREDIT,
                },
            )
            .await?;
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::EventBatch {
                        stream: 2,
                        channel: Channel::ResultValues,
                        events,
                    } if events.len() == 1
                        && events[0].sequence == 2
                        && events[0].event == Event::Value(RuntimeValue::Boolean(true))
                ),
                "protocol-3 socket did not resume with the second ordered SERVER row",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 2 },
                "protocol-3 socket did not complete after every flow-controlled SERVER row",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 3,
                    function: client_function,
                },
                ClientFrame::CallArgument {
                    stream: 3,
                    parameter: orna_core::ParameterId::from_bytes([0x74; 16]),
                    value: record.clone(),
                },
                ClientFrame::CallArgumentsComplete { stream: 3 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 3, .. }
                ),
                "local raw socket did not accept the active-revision record call",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 3,
                        failure: CallFailure::TargetUnavailable,
                    },
                "local raw socket did not retain the closed record-call dispatch boundary",
            )?;

            async {
                let changed_source =
                    RAW_CLIENT_FUNCTION_SOURCE.replace("'lead', 'qualified'", "'lead', 'stale'");
                let changed_bundle =
                    SourceBundle::new([SourceUnit::new("main.orna", changed_source)])?;
                let changed_report = check_standard_application(
                    &changed_bundle,
                    &StandardApplicationCheckContext::try_new(
                        active.catalogue(),
                        standard_upgrade.checked_standard_library(),
                    )?,
                );
                require(
                    changed_report.diagnostics().is_empty(),
                    "stale record preflight fixture did not compile",
                )?;
                let changed = kernel
                    .apply(&prepare_standard_application(
                        &changed_report,
                        active.pair(),
                        &active,
                    )?)
                    .await?;
                current_pair = changed.pair();
                for frame in [
                    ClientFrame::CallRawStart {
                        stream: 4,
                        function: client_function,
                    },
                    ClientFrame::CallArgument {
                        stream: 4,
                        parameter: orna_core::ParameterId::from_bytes([0x74; 16]),
                        value: record,
                    },
                    ClientFrame::CallArgumentsComplete { stream: 4 },
                ] {
                    send_active_protocol_frame(&mut client, &active, &frame).await?;
                }
                require(
                    matches!(
                        read_active_protocol_frame(&mut client, &active).await?,
                        ServerFrame::CallAccepted { stream: 4, .. }
                    ),
                    "local raw socket did not accept the stale record call",
                )?;
                require(
                    read_active_protocol_frame(&mut client, &active).await?
                        == ServerFrame::CallFailed {
                            stream: 4,
                            failure: CallFailure::TargetUnavailable,
                        },
                    "local raw socket did not close stale record dispatch",
                )
            }
            .await
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let cleanup = finish_session(shutdown, connection, "protocol-3 connection cleanup");
        finish_session(
            version_three_operation,
            cleanup,
            "local raw socket protocol-3 operation",
        )?;
        require(
            server_value_wires.len() == 3
                && server_value_wires
                    .windows(2)
                    .all(|values| values[0] == values[1]),
            "protocol-1, protocol-2, and protocol-3 raw SERVER values differ after their exact marker",
        )?;

        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 8
                && events[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[0].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[0].decision().target().is_none()
                && events[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[1].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[1].decision().target()
                    == Some(InvocationTarget::new(client_function, active.pair()))
                && events[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[2].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[2].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[3].decision().target().is_none()
                && events[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[4].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[5].decision().target().is_none()
                && events[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[6].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[7].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair())),
            "local raw socket changed the exact authentication and execute audit sequence",
        )?;

        let opaque_payload = [0x81; 16];
        let current = kernel.recover().await?;
        let opaque_active = install_opaque_client_fixture(
            &kernel,
            &current,
            standard_upgrade.checked_standard_library(),
            client_function,
            opaque_payload,
        )
        .await?;
        current_pair = opaque_active.pair();
        let opaque_granted = SecuritySnapshot::new_with_local_peer_credentials(
            current_pair,
            functions.clone(),
            granted.principals().collect(),
            vec![],
            granted.execute_grants().collect(),
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&opaque_granted).await?;
        let registry = registered_opaque_codecs(
            opaque_active
                .catalogue_hash_context()
                .standard()
                .ok_or_else(|| failure("opaque CLIENT active revision omitted its standard"))?,
        )?;
        let expected_opaque = RuntimeValue::Opaque(OpaqueValue::new(
            &opaque_active,
            &registry,
            OPAQUE_TOKEN_TYPE_ID,
            opaque_payload,
        )?);
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let version_four_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x04\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x04\x00\x00\x00\x00",
                "local raw socket returned the wrong registered-codec acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: client_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 57,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_registered_protocol_frame(&mut client, &opaque_active, &registry, &frame)
                    .await?;
            }
            require(
                matches!(
                    read_registered_protocol_frame(&mut client, &opaque_active, &registry).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "protocol-4 socket did not accept the opaque CLIENT call",
            )?;
            require(
                read_registered_protocol_frame(&mut client, &opaque_active, &registry).await?
                    == ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events: vec![orna_protocol::EventRecord {
                            sequence: 1,
                            event: Event::Value(expected_opaque),
                        }],
                    },
                "protocol-4 socket returned the wrong registered opaque value",
            )?;
            require(
                read_registered_protocol_frame(&mut client, &opaque_active, &registry).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "protocol-4 socket did not complete the opaque CLIENT call",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let cleanup = finish_session(shutdown, connection, "protocol-4 connection cleanup");
        finish_session(
            version_four_operation,
            cleanup,
            "local raw socket protocol-4 operation",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 10
                && events[8].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[8].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[8].decision().target().is_none()
                && events[9].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[9].decision().session_principal() == Some(RAW_CLIENT_USER)
                && events[9].decision().target()
                    == Some(InvocationTarget::new(client_function, current_pair)),
            "protocol-4 opaque CLIENT call changed protected audit evidence",
        )?;

        let revoked = SecuritySnapshot::new(
            current_pair,
            functions,
            granted.principals().collect(),
            vec![],
            granted.execute_grants().collect(),
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let rejected = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let wire = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00")
                .await?;
            let mut response = [0_u8; 1];
            require(
                client.read(&mut response).await? == 0,
                "revoked local peer received bytes instead of a silent close",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let rejection = rejected.await?;
        finish_session(wire, shutdown, "revoked local raw socket cleanup")?;
        require(
            matches!(
                rejection,
                Err(LocalRawSocketError::Authentication {
                    source: LocalAuthenticationError::Kernel {
                        source: PostgresKernelError::LocalPeerAuthentication(
                            LocalPeerAuthenticationError::UnknownUid
                        )
                    }
                })
            ),
            "revoked local peer returned the wrong typed authentication rejection",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 11
                && events[10].decision().kind() == SecurityAuditKind::Authentication
                && events[10].decision().outcome() == SecurityAuditOutcome::Denied
                && events[10].decision().session_principal().is_none()
                && events[10].decision().target().is_none()
                && events[10].decision().denial()
                    == Some(SecurityAuditDenial::Authentication(
                        LocalPeerAuthenticationError::UnknownUid,
                    )),
            "revoked local peer changed the exact denied authentication audit evidence",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn protocol_five_socket_retains_legacy_values_and_closes_constructed_arguments()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        insert_raw_server_flag(&database, &active, 0x81, true).await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let uid = nix::unistd::getuid().as_raw();
        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client_function),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let registry = registered_opaque_codecs(
            active
                .catalogue_hash_context()
                .standard()
                .ok_or_else(|| failure("protocol-5 fixture has no selected standard context"))?,
        )?;
        let boolean_type = TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let constructed_descriptor = TypeDescriptor::list(TypeDescriptor::named(boolean_type))
            .expect("the fixed Boolean LIST descriptor is within the specified limits");
        let constructed_value = RuntimeValue::list(
            &active,
            constructed_descriptor.clone(),
            vec![RuntimeValue::Boolean(true)],
        )?;
        let constructed_rejection = orna_protocol::FrameCodecError::ConstructedValueNotAccepted {
            descriptor: constructed_descriptor.clone(),
        };
        let mut protocol = ProtocolConnection::new();
        protocol.receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 9,
                function: server_function,
            },
        )?;
        protocol.receive_constructed(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 9,
                channel: Channel::ResultValues,
                credit: 1024,
            },
        )?;
        let before_argument = protocol.clone();
        require(
            protocol.receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgument {
                    stream: 9,
                    parameter: ParameterId::from_bytes([0x82; 16]),
                    value: constructed_value.clone(),
                },
            ) == Err(ConnectionError::InvalidFrame {
                source: constructed_rejection.clone(),
            }) && protocol == before_argument,
            "constructed protocol-5 argument changed state or result credit",
        )?;
        require(
            matches!(
                protocol.receive_constructed(
                    &active,
                    &registry,
                    ClientFrame::CallArgumentsComplete { stream: 9 },
                )?,
                Some(orna_protocol::ClientAction::Dispatch { stream: 9, .. })
            ),
            "protocol-5 connection did not retain its callable state after constructed rejection",
        )?;
        protocol.apply_constructed(
            &active,
            &registry,
            ServerAction::Accepted {
                stream: 9,
                invocation: InvocationId::from_bytes([0x83; 16]),
            },
        )?;
        let before_result = protocol.clone();
        require(
            protocol.apply_constructed(
                &active,
                &registry,
                ServerAction::Events {
                    stream: 9,
                    events: vec![Event::Value(constructed_value)],
                },
            ) == Err(ConnectionError::InvalidFrame {
                source: constructed_rejection,
            }) && protocol == before_result,
            "constructed protocol-5 result changed state or result credit",
        )?;
        require(
            matches!(
                protocol.apply_constructed(
                    &active,
                    &registry,
                    ServerAction::Events {
                        stream: 9,
                        events: vec![Event::Value(RuntimeValue::Boolean(true))],
                    },
                )?,
                ServerFrame::EventBatch { stream: 9, .. }
            ),
            "protocol-5 result credit was not retained after constructed-result rejection",
        )?;
        for hello in [
            *b"ORNA\x01\x00\x00\x05\x00\x01\x00\x00",
            *b"ORNA\x01\x01\x00\x05\x00\x00\x00\x00",
            *b"ORNA\x01\x00\x00\x05\x00\x00\x00\x01",
            *b"ORNA\x01\x00\x00\x06\x00\x00\x00\x00",
        ] {
            require_invalid_local_raw_hello(&kernel, hello).await?;
        }

        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let legacy_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "protocol-5 local raw socket returned the wrong acknowledgement",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: server_function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_constructed_protocol_frame(&mut client, &active, &registry, &frame).await?;
            }
            require(
                matches!(
                    read_constructed_protocol_frame(&mut client, &active, &registry).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "protocol-5 socket did not accept the legacy SERVER call",
            )?;
            require(
                read_constructed_protocol_frame(&mut client, &active, &registry).await?
                    == ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events: vec![orna_protocol::EventRecord {
                            sequence: 1,
                            event: Event::Value(RuntimeValue::Boolean(true)),
                        }],
                    },
                "protocol-5 socket did not retain its legacy Boolean result",
            )?;
            require(
                read_constructed_protocol_frame(&mut client, &active, &registry).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "protocol-5 socket did not complete its legacy SERVER call",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: client_function,
                },
                ClientFrame::CallArgument {
                    stream: 2,
                    parameter: ParameterId::from_bytes([0x74; 16]),
                    value: raw_client_record(&active)?,
                },
                ClientFrame::CallArgumentsComplete { stream: 2 },
            ] {
                send_constructed_protocol_frame(&mut client, &active, &registry, &frame).await?;
            }
            require(
                matches!(
                    read_constructed_protocol_frame(&mut client, &active, &registry).await?,
                    ServerFrame::CallAccepted { stream: 2, .. }
                ),
                "protocol-5 socket did not accept the legacy application argument",
            )?;
            require(
                read_constructed_protocol_frame(&mut client, &active, &registry).await?
                    == ServerFrame::CallFailed {
                        stream: 2,
                        failure: CallFailure::TargetUnavailable,
                    },
                "protocol-5 socket did not retain the closed application target boundary",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            legacy_operation,
            finish_session(shutdown, connection, "protocol-5 legacy socket cleanup"),
            "protocol-5 legacy socket operation",
        )?;

        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let rejected = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let constructed_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "constructed-value socket returned the wrong protocol-5 acknowledgement",
            )?;
            send_constructed_protocol_frame(
                &mut client,
                &active,
                &registry,
                &ClientFrame::CallRawStart {
                    stream: 3,
                    function: server_function,
                },
            )
            .await?;
            send_constructed_protocol_frame(
                &mut client,
                &active,
                &registry,
                &ClientFrame::WindowUpdate {
                    stream: 3,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
            )
            .await?;
            client
                .write_all(&constructed_list_argument_frame(
                    3,
                    ParameterId::from_bytes([0x82; 16]),
                    boolean_type,
                ))
                .await?;
            let mut response = [0_u8; 1];
            require(
                timeout(Duration::from_secs(1), client.read(&mut response)).await?? == 0,
                "constructed protocol-5 argument returned a partial server frame",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let rejection = rejected.await?;
        finish_session(
            constructed_operation,
            shutdown,
            "constructed protocol-5 socket cleanup",
        )?;
        require(
            matches!(
                rejection,
                Err(LocalRawSocketError::Frame {
                    source: orna_protocol::FrameCodecError::ConstructedValueNotAccepted {
                        descriptor,
                    },
                }) if descriptor == constructed_descriptor
            ),
            "constructed protocol-5 argument did not close at the public frame boundary",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}
