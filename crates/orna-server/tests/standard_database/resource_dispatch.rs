use super::*;

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn raw_argument_authority_denies_then_grants_and_audits_each_dispatch() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, _client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        let flag_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "flag"])
            .ok_or_else(|| failure("the raw argument fixture is missing app.flag"))?
            .id();
        let create_flagged = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_flagged"])
            .ok_or_else(|| failure("the raw argument fixture is missing app.create_flagged"))?
            .id();
        let p_value = active
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?
            .parameter_by_name("p_value")
            .ok_or_else(|| failure("create_flagged.p_value is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = p_value.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != p_value,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();

        // A read-only grant denies the parameterised INSERT before any row.
        let read_only = SecuritySnapshot::new(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, server_function)],
        )?;
        let security = kernel.replace_security_snapshot(&read_only).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        let denied = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            1,
            RawCall {
                function: create_flagged,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_value,
                    value: RuntimeValue::Boolean(true),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &denied,
            1,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted raw argument INSERT was not denied before dispatch",
        )?;
        let empty = RawClientDispatch::new(kernel.clone(), session, 2, raw_call(server_function))
            .finish()
            .await;
        require(
            empty.source().is_none() && empty.actions() == [ServerAction::Completed { stream: 2 }],
            "the denied raw argument INSERT must leave the read empty",
        )?;

        // Grant the INSERT and the read together, then bind a fresh session.
        let granted = SecuritySnapshot::new(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&granted).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // TRUE with the exact discovered ParameterId returns one reference.
        let inserted_true = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            3,
            RawCall {
                function: create_flagged,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_value,
                    value: RuntimeValue::Boolean(true),
                }],
            },
        )
        .finish()
        .await;
        let [
            ServerAction::Events { stream: 3, events },
            ServerAction::Completed { stream: 3 },
        ] = inserted_true.actions()
        else {
            return Err(failure(
                "the TRUE raw argument INSERT must return one event batch and completion",
            ));
        };
        let [Event::Value(RuntimeValue::Reference { target, object })] = events.as_slice() else {
            return Err(failure(
                "the TRUE raw argument INSERT did not return one reference",
            ));
        };
        require(
            *target == flag_type && *object != ObjectId::from_bytes([0; 16]),
            "the TRUE raw argument INSERT returned the wrong reference",
        )?;
        let true_object = *object;

        // The parameter-free read observes the stored TRUE row.
        let read_true = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            4,
            raw_call(server_function),
        )
        .finish()
        .await;
        require(
            read_true.source().is_none()
                && read_true.actions()
                    == [
                        ServerAction::Events {
                            stream: 4,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 4 },
                    ],
            "the TRUE raw argument INSERT did not become visible to the read",
        )?;

        // A wrong ParameterId closes as TARGET_UNAVAILABLE with a retained
        // private create_flagged source, stays non-operational under
        // cancellation, and adds no row.
        let wrong_target = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            5,
            RawCall {
                function: create_flagged,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: RuntimeValue::Boolean(true),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &wrong_target,
            5,
            CallFailure::TargetUnavailable,
            matches!(
                wrong_target.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == create_flagged
            ),
            "a wrong raw argument ParameterId did not close as an unavailable target",
        )?;
        require(
            wrong_target.action_after_cancellation() == ServerAction::Cancelled { stream: 5 },
            "a wrong raw argument ParameterId must remain non-operational under cancellation",
        )?;
        let unchanged = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            6,
            raw_call(server_function),
        )
        .finish()
        .await;
        require(
            unchanged.source().is_none()
                && unchanged.actions()
                    == [
                        ServerAction::Events {
                            stream: 6,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 6 },
                    ],
            "a wrong raw argument ParameterId must not add any row",
        )?;

        // FALSE with the exact discovered ParameterId returns a second
        // distinct nonzero object reference for the same app.flag type.
        let inserted_false = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            7,
            RawCall {
                function: create_flagged,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_value,
                    value: RuntimeValue::Boolean(false),
                }],
            },
        )
        .finish()
        .await;
        let [
            ServerAction::Events { stream: 7, events },
            ServerAction::Completed { stream: 7 },
        ] = inserted_false.actions()
        else {
            return Err(failure(
                "the FALSE raw argument INSERT must return one event batch and completion",
            ));
        };
        let [Event::Value(RuntimeValue::Reference { target, object })] = events.as_slice() else {
            return Err(failure(
                "the FALSE raw argument INSERT did not return one reference",
            ));
        };
        require(
            *target == flag_type
                && *object != ObjectId::from_bytes([0; 16])
                && *object != true_object,
            "the FALSE raw argument INSERT returned the wrong distinct reference",
        )?;

        // The read returns exactly one TRUE and one FALSE in no particular
        // row order.
        let read_both =
            RawClientDispatch::new(kernel.clone(), session, 8, raw_call(server_function))
                .finish()
                .await;
        let [
            ServerAction::Events { stream: 8, events },
            ServerAction::Events {
                stream: 8,
                events: second_events,
            },
            ServerAction::Completed { stream: 8 },
        ] = read_both.actions()
        else {
            return Err(failure(
                "the argument SELECT must return two event batches and completion",
            ));
        };
        let [Event::Value(first_value)] = events.as_slice() else {
            return Err(failure(
                "the argument SELECT did not return one first value",
            ));
        };
        let [Event::Value(second_value)] = second_events.as_slice() else {
            return Err(failure(
                "the argument SELECT did not return one second value",
            ));
        };
        let ordered_ok = (first_value == &RuntimeValue::Boolean(true)
            && second_value == &RuntimeValue::Boolean(false))
            || (first_value == &RuntimeValue::Boolean(false)
                && second_value == &RuntimeValue::Boolean(true));
        require(
            ordered_ok,
            "the argument SELECT must return exactly one TRUE and one FALSE in any order",
        )?;

        // Eight execute audits in dispatch order: the pre-grant denial, then
        // every allowed dispatch including the wrong-parameter closure whose
        // allowed audit survived its savepoint rollback.
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 8,
            "raw argument authority audit count differs",
        )?;
        require(
            events[0].decision().kind() == SecurityAuditKind::Execute
                && events[0].decision().outcome() == SecurityAuditOutcome::Denied
                && events[0].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && events[0].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[1].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[2].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[3].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[4].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[5].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[6].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[7].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair())),
            "raw argument authority changed the exact durable audit sequence",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

/// Proves the raw socket-facing dispatch boundary exposes only the approved
/// version-2 identity-selected SERVER SELECT form.
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn raw_identity_selected_server_read_authorises_binds_and_redacts() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("raw-identity-live".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "raw identity live runtime could not start: {error}"
                    ))
                })?;
            runtime.block_on(raw_identity_selected_server_read_authorises_binds_and_redacts_inner())
        })
        .map_err(|error| failure(format!("raw identity live thread could not start: {error}")))?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("raw identity live thread panicked")),
    }
}

async fn raw_identity_selected_server_read_authorises_binds_and_redacts_inner() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, _client_function, legacy_read) =
            install_raw_client_fixture(&kernel).await?;
        let flag_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "flag"])
            .ok_or_else(|| failure("the raw identity fixture is missing app.flag"))?
            .id();
        let create_flagged = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_flagged"])
            .ok_or_else(|| failure("the raw identity fixture is missing app.create_flagged"))?
            .id();
        let select_flag = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "select_flag"])
            .ok_or_else(|| failure("the raw identity fixture is missing app.select_flag"))?
            .id();
        let create_parameter = active
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?
            .parameter_by_name("p_value")
            .ok_or_else(|| failure("create_flagged.p_value is absent from the active catalogue"))?
            .id();
        let select_parameter = active
            .catalogue()
            .function_by_id(select_flag)
            .ok_or_else(|| failure("select_flag is absent from the active catalogue"))?
            .parameter_by_name("p_flag")
            .ok_or_else(|| failure("select_flag.p_flag is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = select_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != select_parameter,
            "the deliberately wrong identity-read parameter must differ from the declaration",
        )?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();

        // Create one selected object before the selector grant exists.
        let writer_only = SecuritySnapshot::new(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, create_flagged)],
        )?;
        let security = kernel.replace_security_snapshot(&writer_only).await?;
        let writer = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let reference = create_flag_reference(
            &kernel,
            &writer,
            create_flagged,
            create_parameter,
            flag_type,
            1,
        )
        .await?;

        // The protocol only exposes the public denial. The private cause
        // proves authorisation occurred before reference binding.
        let denied = RawClientDispatch::new(
            kernel.clone(),
            writer,
            2,
            RawCall {
                function: select_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: select_parameter,
                    value: reference.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &denied,
            2,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted identity-selected raw read was not redacted",
        )?;

        let granted = SecuritySnapshot::new(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, legacy_read),
                ExecuteGrant::new(RAW_CLIENT_USER, select_flag),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&granted).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // The selected row flattens its two ORF1 values in declared column
        // order, then emits the normal completion action.
        let selected = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            3,
            RawCall {
                function: select_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: select_parameter,
                    value: reference.clone(),
                }],
            },
        )
        .finish()
        .await;
        let expected_selected = [
            ServerAction::Events {
                stream: 3,
                events: vec![Event::Value(reference.clone())],
            },
            ServerAction::Events {
                stream: 3,
                events: vec![Event::Value(RuntimeValue::Boolean(true))],
            },
            ServerAction::Completed { stream: 3 },
        ];
        require(
            selected.source().is_none() && selected.actions() == expected_selected,
            "the identity-selected raw read did not preserve projected ORF1 value order",
        )?;

        let absent = RuntimeValue::Reference {
            target: flag_type,
            object: ObjectId::from_bytes([0x6d; 16]),
        };
        let no_row = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            4,
            RawCall {
                function: select_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: select_parameter,
                    value: absent.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            no_row.source().is_none()
                && no_row.actions() == [ServerAction::Completed { stream: 4 }],
            "an absent same-type reference must complete without raw values",
        )?;

        let wrong = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            5,
            RawCall {
                function: select_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: reference.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &wrong,
            5,
            CallFailure::TargetUnavailable,
            matches!(
                wrong.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == select_flag
            ),
            "a wrong identity-selected ParameterId did not close as unavailable",
        )?;

        // The existing zero-argument read remains available, but a Reference
        // cannot open its version-1 path.
        let legacy =
            RawClientDispatch::new(kernel.clone(), session.clone(), 6, raw_call(legacy_read))
                .finish()
                .await;
        require(
            legacy.source().is_none()
                && legacy.actions()
                    == [
                        ServerAction::Events {
                            stream: 6,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 6 },
                    ],
            "the legacy parameter-free raw read changed during identity selection",
        )?;
        let legacy_argument = RawClientDispatch::new(
            kernel.clone(),
            session,
            7,
            RawCall {
                function: legacy_read,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: select_parameter,
                    value: absent.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &legacy_argument,
            7,
            CallFailure::TargetUnavailable,
            matches!(
                legacy_argument.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == legacy_read
            ),
            "a Reference argument opened the legacy raw read path",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 7
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[0].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[1].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && audits[1].decision().target()
                    == Some(InvocationTarget::new(select_flag, active.pair()))
                && audits[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[2].decision().target()
                    == Some(InvocationTarget::new(select_flag, active.pair()))
                && audits[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[3].decision().target()
                    == Some(InvocationTarget::new(select_flag, active.pair()))
                && audits[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[4].decision().target()
                    == Some(InvocationTarget::new(select_flag, active.pair()))
                && audits[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[5].decision().target()
                    == Some(InvocationTarget::new(legacy_read, active.pair()))
                && audits[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[6].decision().target()
                    == Some(InvocationTarget::new(legacy_read, active.pair())),
            "identity-selected raw read changed its durable authorisation audit sequence",
        )?;

        // Exercise the same public closure through the local authenticated
        // socket. The direct dispatcher calls above retain the private typed
        // causes; these frames prove that the protocol does not disclose them.
        let uid = nix::unistd::getuid().as_raw();
        let socket_functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let denied_socket_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            socket_functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel
            .replace_security_snapshot(&denied_socket_security)
            .await?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let denied_socket_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "identity-selected socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter: select_parameter,
                    value: reference.clone(),
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
                "identity-selected socket did not accept the denied call",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 1,
                        failure: CallFailure::ExecuteDenied,
                    },
                "identity-selected socket disclosed or changed ExecuteDenied",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_socket_operation,
            finish_session(
                shutdown,
                connection,
                "identity-selected denied socket cleanup",
            ),
            "identity-selected denied socket operation",
        )?;

        let granted_socket_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            socket_functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, select_flag)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel
            .replace_security_snapshot(&granted_socket_security)
            .await?;
        let reference_event_credit = u64::try_from(
            encode_active_server_frame(
                &active,
                &ServerFrame::EventBatch {
                    stream: 2,
                    channel: Channel::ResultValues,
                    events: vec![orna_protocol::EventRecord {
                        sequence: 1,
                        event: Event::Value(reference.clone()),
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
        let granted_socket_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00",
                "identity-selected granted socket returned the wrong acknowledgement",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 2,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 2,
                    parameter: select_parameter,
                    value: reference.clone(),
                },
                ClientFrame::WindowUpdate {
                    stream: 2,
                    channel: Channel::ResultValues,
                    credit: reference_event_credit,
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
                "identity-selected socket did not accept the granted call",
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
                        && events[0].event == Event::Value(reference.clone())
                ),
                "identity-selected socket did not emit the first projected Reference",
            )?;
            sleep(Duration::from_millis(50)).await;
            let mut unexpected = [0_u8; 1];
            require(
                matches!(
                    client.try_read(&mut unexpected),
                    Err(error) if error.kind() == ErrorKind::WouldBlock
                ),
                "identity-selected socket emitted its second projection without byte credit",
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
                "identity-selected socket did not resume with the Boolean projection",
            )?;
            require(
                read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 2 },
                "identity-selected socket did not complete after both projected values",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 3,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 3,
                    parameter: select_parameter,
                    value: absent.clone(),
                },
                ClientFrame::CallArgumentsComplete { stream: 3 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 3, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCompleted { stream: 3 },
                "identity-selected socket did not close an absent reference without values",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 4,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 4,
                    parameter: wrong_parameter,
                    value: reference.clone(),
                },
                ClientFrame::CallArgumentsComplete { stream: 4 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 4, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallFailed {
                        stream: 4,
                        failure: CallFailure::TargetUnavailable,
                    },
                "identity-selected socket disclosed or changed TargetUnavailable",
            )?;

            for frame in [
                ClientFrame::CallRawStart {
                    stream: 5,
                    function: select_flag,
                },
                ClientFrame::CallArgument {
                    stream: 5,
                    parameter: select_parameter,
                    value: reference,
                },
                ClientFrame::CallArgumentsComplete { stream: 5 },
                ClientFrame::CallCancel { stream: 5 },
            ] {
                send_active_protocol_frame(&mut client, &active, &frame).await?;
            }
            require(
                matches!(
                    read_active_protocol_frame(&mut client, &active).await?,
                    ServerFrame::CallAccepted { stream: 5, .. }
                ) && read_active_protocol_frame(&mut client, &active).await?
                    == ServerFrame::CallCancelled { stream: 5 },
                "identity-selected socket did not close the cancelled reference call",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            granted_socket_operation,
            finish_session(
                shutdown,
                connection,
                "identity-selected granted socket cleanup",
            ),
            "identity-selected granted socket operation",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn server_raw_reference_mutation_authority_selection_and_audit() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("raw-reference-mutation-live".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "raw reference mutation live runtime could not start: {error}"
                    ))
                })?;
            runtime.block_on(server_raw_reference_mutation_authority_selection_and_audit_inner())
        })
        .map_err(|error| {
            failure(format!(
                "raw reference mutation live thread could not start: {error}"
            ))
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("raw reference mutation live thread panicked")),
    }
}

async fn server_raw_reference_mutation_authority_selection_and_audit_inner() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, _client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        let flag_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "flag"])
            .ok_or_else(|| failure("the reference fixture is missing app.flag"))?
            .id();
        let create_flagged = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_flagged"])
            .ok_or_else(|| failure("the reference fixture is missing app.create_flagged"))?
            .id();
        let update_false = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "update_false"])
            .ok_or_else(|| failure("the reference fixture is missing app.update_false"))?
            .id();
        let delete_flag = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "delete_flag"])
            .ok_or_else(|| failure("the reference fixture is missing app.delete_flag"))?
            .id();
        let p_value = active
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?
            .parameter_by_name("p_value")
            .ok_or_else(|| failure("create_flagged.p_value is absent from the active catalogue"))?
            .id();
        let p_flag = active
            .catalogue()
            .function_by_id(update_false)
            .ok_or_else(|| failure("update_false is absent from the active catalogue"))?
            .parameter_by_name("p_flag")
            .ok_or_else(|| failure("update_false.p_flag is absent from the active catalogue"))?
            .id();
        let delete_parameter = active
            .catalogue()
            .function_by_id(delete_flag)
            .ok_or_else(|| failure("delete_flag is absent from the active catalogue"))?
            .parameter_by_name("p_flag")
            .ok_or_else(|| failure("delete_flag.p_flag is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = p_flag.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != p_flag,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();

        // Grant only the writer and the reader; the reference mutations stay
        // unauthorised for the denial proof.
        let read_only = SecuritySnapshot::new(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&read_only).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // Create two distinct rows and retain both exact references.
        let first =
            create_flag_reference(&kernel, &session, create_flagged, p_value, flag_type, 1).await?;
        let second =
            create_flag_reference(&kernel, &session, create_flagged, p_value, flag_type, 2).await?;
        require(
            first != second,
            "the two created references must be distinct",
        )?;

        // The identical invalid binding is denied before its grant, proving
        // authorisation precedes argument binding.
        let denied = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            3,
            RawCall {
                function: update_false,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: second.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &denied,
            3,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted invalid-binding UPDATE was not denied before dispatch",
        )?;

        // Grant create, read, update, and delete, then bind a fresh session.
        let granted = SecuritySnapshot::new(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
                ExecuteGrant::new(RAW_CLIENT_USER, update_false),
                ExecuteGrant::new(RAW_CLIENT_USER, delete_flag),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&granted).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // UPDATE selects the first row and returns the identical reference.
        let updated = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            4,
            RawCall {
                function: update_false,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_flag,
                    value: first.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            updated.source().is_none(),
            "the Reference UPDATE must not retain a kernel source",
        )?;
        let [
            ServerAction::Events { stream: 4, events },
            ServerAction::Completed { stream: 4 },
        ] = updated.actions()
        else {
            return Err(failure(
                "the Reference UPDATE must return one event batch and completion",
            ));
        };
        let [Event::Value(updated_reference)] = events.as_slice() else {
            return Err(failure("the Reference UPDATE must return one reference"));
        };
        require(
            *updated_reference == first,
            "the Reference UPDATE must return exactly the identical input reference",
        )?;

        // The reader returns exactly one FALSE and one TRUE in no row order.
        let mixed = read_flag_values(&kernel, &session, server_function, 5).await?;
        require(
            mixed.len() == 2
                && mixed
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1
                && mixed
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 1,
            "the Reference UPDATE must select exactly one row",
        )?;

        // The same invalid binding closes as unavailable after the grant, and
        // the preserved read proves the second row stayed TRUE: an erroneous
        // post-grant execution would have made it FALSE.
        let wrong = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            6,
            RawCall {
                function: update_false,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: second.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &wrong,
            6,
            CallFailure::TargetUnavailable,
            matches!(
                wrong.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == update_false
            ),
            "a wrong UPDATE ParameterId did not close as an unavailable target",
        )?;
        let preserved = read_flag_values(&kernel, &session, server_function, 7).await?;
        require(
            preserved.len() == 2
                && preserved
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1
                && preserved
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 1,
            "the wrong UPDATE ParameterId must preserve both rows",
        )?;

        // DELETE the first row and prove the reader keeps only the second.
        let deleted = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            8,
            RawCall {
                function: delete_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: delete_parameter,
                    value: first.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            deleted.source().is_none()
                && deleted.actions()
                    == [
                        ServerAction::Events {
                            stream: 8,
                            events: vec![Event::Value(RuntimeValue::Boolean(true))],
                        },
                        ServerAction::Completed { stream: 8 },
                    ],
            "the reference DELETE must return exactly one TRUE value",
        )?;
        let one_true = read_flag_values(&kernel, &session, server_function, 9).await?;
        require(
            one_true == [RuntimeValue::Boolean(true)],
            "the reference DELETE must leave exactly the second row TRUE",
        )?;

        // Repeated DELETE and UPDATE of the deleted reference both complete
        // with no value events.
        let repeated = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            10,
            RawCall {
                function: delete_flag,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: delete_parameter,
                    value: first.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            repeated.source().is_none()
                && repeated.actions() == [ServerAction::Completed { stream: 10 }],
            "the repeated reference DELETE must complete with no value events",
        )?;
        let deleted_update = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            11,
            RawCall {
                function: update_false,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_flag,
                    value: first.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            deleted_update.source().is_none()
                && deleted_update.actions() == [ServerAction::Completed { stream: 11 }],
            "the UPDATE of the deleted reference must complete with no value events",
        )?;

        // The final read shows only the second TRUE row.
        let final_read = read_flag_values(&kernel, &session, server_function, 12).await?;
        require(
            final_read == [RuntimeValue::Boolean(true)],
            "the final read must show only the second TRUE row",
        )?;

        // Authentication is session binding here, so every audit is Execute
        // with exact outcomes and targets in dispatch order.
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 12,
            "server reference mutation audit count differs",
        )?;
        require(
            events[0].decision().kind() == SecurityAuditKind::Execute
                && events[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[0].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[1].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[2].decision().outcome() == SecurityAuditOutcome::Denied
                && events[2].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && events[2].decision().target()
                    == Some(InvocationTarget::new(update_false, active.pair()))
                && events[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[3].decision().target()
                    == Some(InvocationTarget::new(update_false, active.pair()))
                && events[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[4].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[5].decision().target()
                    == Some(InvocationTarget::new(update_false, active.pair()))
                && events[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[6].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[7].decision().target()
                    == Some(InvocationTarget::new(delete_flag, active.pair()))
                && events[8].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[8].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair()))
                && events[9].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[9].decision().target()
                    == Some(InvocationTarget::new(delete_flag, active.pair()))
                && events[10].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[10].decision().target()
                    == Some(InvocationTarget::new(update_false, active.pair()))
                && events[11].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[11].decision().target()
                    == Some(InvocationTarget::new(server_function, active.pair())),
            "server reference mutation changed the exact durable audit sequence",
        )?;

        // The active revision pair is unchanged.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == active.pair(),
            "server reference mutations must not change the active revision pair",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

/// One authenticated raw reference-INSERT authority journey through the
/// public server adapter.
///
/// The test installs the shared raw CLIENT fixture plus the additive
/// `app.assignment` unique-reference pair, discovers every identity from the
/// active catalogue, and creates one real owner reference through the public
/// adapter. A wrong-parameter reference call is denied before its grant and
/// creates no assignment. After the grant, the same wrong parameter closes as
/// `CallFailure::TargetUnavailable` without adding a row, a correct reference
/// call succeeds and returns one assignment reference, and the duplicate call
/// is redacted as public `CallFailure::InternalFailure` while retaining the
/// private typed `UniqueReferenceConflict` source. The public reader exposes
/// exactly one dependent row after the duplicate. The exact audit
/// outcome/target sequence and the unchanged active revision are asserted.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn server_raw_reference_insert_authority() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, _standard_upgrade, _client_function, server_function) =
            install_raw_client_fixture(&kernel).await?;
        let flag_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "flag"])
            .ok_or_else(|| failure("the raw reference INSERT fixture is missing app.flag"))?
            .id();
        let assignment_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["app", "assignment"])
            .ok_or_else(|| failure("the raw reference INSERT fixture is missing app.assignment"))?
            .id();
        let create_flagged = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_flagged"])
            .ok_or_else(|| {
                failure("the raw reference INSERT fixture is missing app.create_flagged")
            })?
            .id();
        let create_assignment = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "create_assignment"])
            .ok_or_else(|| {
                failure("the raw reference INSERT fixture is missing app.create_assignment")
            })?
            .id();
        let read_assignments = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["app", "read_assignments"])
            .ok_or_else(|| {
                failure("the raw reference INSERT fixture is missing app.read_assignments")
            })?
            .id();
        let p_value = active
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?
            .parameter_by_name("p_value")
            .ok_or_else(|| failure("create_flagged.p_value is absent from the active catalogue"))?
            .id();
        let p_flag = active
            .catalogue()
            .function_by_id(create_assignment)
            .ok_or_else(|| failure("create_assignment is absent from the active catalogue"))?
            .parameter_by_name("p_flag")
            .ok_or_else(|| failure("create_assignment.p_flag is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = p_flag.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != p_flag,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();

        // Grant only the owner create, the reader, and the assignment reader;
        // the assignment create stays unauthorised for the denial proof.
        let read_only = SecuritySnapshot::new(
            active.pair(),
            functions.clone(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
                ExecuteGrant::new(RAW_CLIENT_USER, read_assignments),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&read_only).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // One real owner reference through the public adapter.
        let owner =
            create_flag_reference(&kernel, &session, create_flagged, p_value, flag_type, 1).await?;

        // The identical wrong-parameter call is denied before its grant,
        // proving authorisation precedes argument validation.
        let denied = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            2,
            RawCall {
                function: create_assignment,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: owner.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &denied,
            2,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted raw reference INSERT was not denied before dispatch",
        )?;
        let zero_before = read_flag_values(&kernel, &session, read_assignments, 3).await?;
        require(
            zero_before.is_empty(),
            "the denied raw reference INSERT must leave zero assignments",
        )?;

        // Grant the assignment create, then bind a fresh session.
        let granted = SecuritySnapshot::new(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_flagged),
                ExecuteGrant::new(RAW_CLIENT_USER, server_function),
                ExecuteGrant::new(RAW_CLIENT_USER, read_assignments),
                ExecuteGrant::new(RAW_CLIENT_USER, create_assignment),
            ],
        )?;
        let security = kernel.replace_security_snapshot(&granted).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;

        // The wrong parameter closes as an unavailable target after the grant
        // without adding any assignment row.
        let wrong = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            4,
            RawCall {
                function: create_assignment,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: wrong_parameter,
                    value: owner.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &wrong,
            4,
            CallFailure::TargetUnavailable,
            matches!(
                wrong.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == create_assignment
            ),
            "a wrong INSERT ParameterId did not close as an unavailable target",
        )?;
        let zero_after_wrong = read_flag_values(&kernel, &session, read_assignments, 5).await?;
        require(
            zero_after_wrong.is_empty(),
            "the wrong INSERT ParameterId must not add any assignment row",
        )?;

        // The correct reference call succeeds and returns one assignment
        // reference whose target differs from the owner type.
        let created = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            6,
            RawCall {
                function: create_assignment,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_flag,
                    value: owner.clone(),
                }],
            },
        )
        .finish()
        .await;
        require(
            created.source().is_none(),
            "the raw reference INSERT must not retain a kernel source",
        )?;
        let [
            ServerAction::Events {
                stream: events_stream,
                events,
            },
            ServerAction::Completed {
                stream: completed_stream,
            },
        ] = created.actions()
        else {
            return Err(failure(
                "the raw reference INSERT must return one event batch and completion",
            ));
        };
        require(
            *events_stream == 6 && *completed_stream == 6,
            "the raw reference INSERT must use the exact stream",
        )?;
        let [Event::Value(RuntimeValue::Reference { target, object })] = events.as_slice() else {
            return Err(failure(
                "the raw reference INSERT must return one assignment reference",
            ));
        };
        require(
            *target == assignment_type
                && *target != flag_type
                && *object != ObjectId::from_bytes([0; 16]),
            "the assignment reference must name the assignment type and a real nonzero row",
        )?;

        // The duplicate call is redacted as a public internal failure while
        // retaining the private typed unique-reference conflict source.
        let duplicate = RawClientDispatch::new(
            kernel.clone(),
            session.clone(),
            7,
            RawCall {
                function: create_assignment,
                arguments: vec![orna_protocol::CallArgument {
                    parameter: p_flag,
                    value: owner.clone(),
                }],
            },
        )
        .finish()
        .await;
        require_dispatch_failure(
            &duplicate,
            7,
            CallFailure::InternalFailure,
            matches!(
                duplicate.source(),
                Some(PostgresKernelError::ServerInsert(
                    ServerInsertError::NotCommitted { source: inner, .. }
                )) if matches!(inner.as_ref(), ServerMutationError::UniqueReferenceConflict { .. })
            ),
            "the duplicate raw reference INSERT was not redacted with its private conflict source",
        )?;

        // The public reader exposes exactly one dependent row after the
        // duplicate.
        let one = read_flag_values(&kernel, &session, read_assignments, 8).await?;
        require(
            one == [RuntimeValue::Boolean(true)],
            "the public reader must expose exactly one TRUE assignment after the duplicate",
        )?;

        // Authentication is session binding here, so every audit is Execute
        // with exact outcomes and targets in dispatch order.
        let events = kernel.recover_security_audit_events().await?;
        require(
            events.len() == 8
                && events[0].decision().kind() == SecurityAuditKind::Execute
                && events[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[0].decision().target()
                    == Some(InvocationTarget::new(create_flagged, active.pair()))
                && events[1].decision().outcome() == SecurityAuditOutcome::Denied
                && events[1].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && events[1].decision().target()
                    == Some(InvocationTarget::new(create_assignment, active.pair()))
                && events[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[2].decision().target()
                    == Some(InvocationTarget::new(read_assignments, active.pair()))
                && events[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[3].decision().target()
                    == Some(InvocationTarget::new(create_assignment, active.pair()))
                && events[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[4].decision().target()
                    == Some(InvocationTarget::new(read_assignments, active.pair()))
                && events[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[5].decision().target()
                    == Some(InvocationTarget::new(create_assignment, active.pair()))
                && events[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[6].decision().target()
                    == Some(InvocationTarget::new(create_assignment, active.pair()))
                && events[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && events[7].decision().target()
                    == Some(InvocationTarget::new(read_assignments, active.pair())),
            "raw reference INSERT changed the exact durable audit sequence",
        )?;

        // The active revision pair is unchanged.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == active.pair(),
            "raw reference INSERTs must not change the active revision pair",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn server_raw_integer_dispatch_denies_then_grants_and_audits_exact_values() -> TestResult<()>
{
    // One Integer tracer through the public adapter. The PostgreSQL scalar
    // matrix already proves the kernel bind and value contract.
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client_function, _server_function) =
            install_raw_client_fixture(&kernel).await?;
        let (active, int_probe, create_int, create_int_parameter, read_ints) =
            install_raw_int_insert_fixture(&kernel, &active, &standard_upgrade).await?;
        let mut wrong_parameter_bytes = create_int_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let snapshot = |grants: Vec<ExecuteGrant>| {
            SecuritySnapshot::new(
                active.pair(),
                active
                    .catalogue()
                    .functions()
                    .iter()
                    .map(|function| function.id())
                    .collect::<Vec<_>>(),
                vec![principal],
                vec![],
                grants,
            )
            .expect("the raw Integer test security snapshot is valid")
        };
        let dispatch = |session: &AuthenticatedSession,
                        stream: u64,
                        parameter: ParameterId,
                        value: RuntimeValue| {
            RawClientDispatch::new(
                kernel.clone(),
                session.clone(),
                stream,
                RawCall {
                    function: create_int,
                    arguments: vec![orna_protocol::CallArgument { parameter, value }],
                },
            )
        };

        // The wrong-parameter call is denied before its grant, proving
        // authorisation precedes argument validation.
        let read_only = kernel
            .replace_security_snapshot(&snapshot(vec![ExecuteGrant::new(
                RAW_CLIENT_USER,
                read_ints,
            )]))
            .await?;
        let session = read_only.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let denied = dispatch(&session, 1, wrong_parameter, RuntimeValue::Integer(7))
            .finish()
            .await;
        require_dispatch_failure(
            &denied,
            1,
            CallFailure::ExecuteDenied,
            matches!(
                denied.source(),
                Some(PostgresKernelError::RawExecuteDenied {
                    reason: ExecuteDenial::MissingExecuteGrant,
                    ..
                })
            ),
            "an ungranted raw Integer INSERT was not denied before argument binding",
        )?;

        // Grant the INSERT, store one exact value, and read it back.
        let granted = kernel
            .replace_security_snapshot(&snapshot(vec![
                ExecuteGrant::new(RAW_CLIENT_USER, create_int),
                ExecuteGrant::new(RAW_CLIENT_USER, read_ints),
            ]))
            .await?;
        let session = granted.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let inserted = dispatch(&session, 3, create_int_parameter, RuntimeValue::Integer(-7))
            .finish()
            .await;
        let events = match inserted.actions() {
            [
                ServerAction::Events { events, .. },
                ServerAction::Completed { .. },
            ] => events,
            _ => {
                return Err(failure(
                    "the raw Integer INSERT must return one event batch and completion",
                ));
            }
        };
        let [Event::Value(RuntimeValue::Reference { target, object })] = events.as_slice() else {
            return Err(failure(
                "the raw Integer INSERT did not return one reference",
            ));
        };
        require(
            *target == int_probe && *object != ObjectId::from_bytes([0; 16]),
            "the raw Integer INSERT returned the wrong reference",
        )?;

        // The wrong parameter closes redacted as an unavailable target. The
        // final read proves the exact stored value and that neither the
        // denied nor the wrong call added a row.
        let wrong = dispatch(&session, 5, wrong_parameter, RuntimeValue::Integer(9))
            .finish()
            .await;
        require_dispatch_failure(
            &wrong,
            5,
            CallFailure::TargetUnavailable,
            matches!(
                wrong.source(),
                Some(PostgresKernelError::RawCallTargetUnavailable { function, .. })
                    if *function == create_int
            ),
            "a wrong raw Integer ParameterId did not close as an unavailable target",
        )?;
        require(
            read_flag_values(&kernel, &session, read_ints, 6).await? == [RuntimeValue::Integer(-7)],
            "the final read must show exactly the stored value and no extra row",
        )?;

        // Authentication is session binding here, so every audit is Execute
        // with the exact outcome, target, and principal in dispatch order.
        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 4
                && audits.iter().enumerate().all(|(index, event)| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::Execute
                        && decision.session_principal() == Some(RAW_CLIENT_USER)
                        && decision.outcome()
                            == [
                                SecurityAuditOutcome::Denied,
                                SecurityAuditOutcome::Allowed,
                                SecurityAuditOutcome::Allowed,
                                SecurityAuditOutcome::Allowed,
                            ][index]
                        && decision.target().map(InvocationTarget::function)
                            == Some([create_int, create_int, create_int, read_ints][index])
                }),
            "raw Integer dispatch changed the exact durable audit sequence",
        )?;

        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_stream_resource_dispatches_allowed_and_denied_with_redacted_audit()
-> TestResult<()> {
    const RESOURCE_VALUE: &str = "resource-value";
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client_function, _server_function) =
            install_raw_client_fixture(&kernel).await?;
        let (active, _resource_client, target, parameter, call_site) =
            install_stream_resource_client_fixture(
                &kernel,
                &active,
                standard_upgrade.checked_standard_library(),
            )
            .await
            .map_err(|error| {
                failure(format!("install stream resource fixture failed: {error:?}"))
            })?;
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "create"])
            .ok_or_else(|| failure("stream resource fixture is missing resource_fixture.create"))?
            .id();
        let create_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource_fixture.create is absent from the active catalogue"))?
            .parameter_by_name("p_marker")
            .ok_or_else(|| {
                failure("resource_fixture.create.p_marker is absent from the active catalogue")
            })?
            .id();
        let sequence_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource_fixture.create is absent from the active catalogue"))?
            .parameter_by_name("p_sequence")
            .ok_or_else(|| {
                failure("resource_fixture.create.p_sequence is absent from the active catalogue")
            })?
            .id();
        kernel
            .execute_server_insert(
                create,
                &[
                    FunctionArgument::new(
                        create_parameter,
                        RuntimeValue::Text(RESOURCE_VALUE.into()),
                    )?,
                    FunctionArgument::new(sequence_parameter, RuntimeValue::Integer(1))?,
                ],
            )
            .await
            .map_err(|error| {
                failure(format!(
                    "insert stream resource fixture row failed: {error:?}"
                ))
            })?;
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let mut request = ResourceRequest {
            stream_id: 73,
            request_id: InvocationId::from_bytes([0x31; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x32; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text(RESOURCE_VALUE.into()),
            }],
            item_window: 1,
            byte_window: 1024,
        };
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let snapshot = |grants| {
            SecuritySnapshot::new_with_function_targets(
                active.pair(),
                functions
                    .iter()
                    .copied()
                    .map(SecurityFunctionTarget::application)
                    .collect(),
                vec![principal],
                vec![],
                grants,
            )
            .expect("the stream resource security snapshot is valid")
        };
        let allowed = kernel
            .replace_security_snapshot(&snapshot(vec![ExecuteGrant::new(RAW_CLIENT_USER, target)]))
            .await?;
        let session = allowed.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let parent_request =
            sealed_scalar_resource_request(SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID)?;
        let parent =
            create_authenticated_parent_invocation(&kernel, &active, &session, parent_request)
                .await?;
        request.parent_invocation_id = parent;
        let completed = kernel
            .dispatch_authenticated_server_resource(&session, &request)
            .await?;
        let nested = match completed {
            AuthenticatedServerResourceResult::Completed {
                stream_id,
                request_id,
                nested_invocation_id,
                target_revision,
                resource_kind,
                values,
            } => {
                require(
                    stream_id == request.stream_id,
                    "stream resource changed stream identity",
                )?;
                require(
                    request_id == request.request_id,
                    "stream resource changed request identity",
                )?;
                require(
                    target_revision == active.pair(),
                    "stream resource changed active revision",
                )?;
                require(
                    resource_kind == ResourceKind::Stream,
                    "stream resource changed result kind",
                )?;
                require(
                    values == [RuntimeValue::Text(RESOURCE_VALUE.into())],
                    "stream resource returned the wrong text",
                )?;
                require(
                    nested_invocation_id != InvocationId::from_bytes([0; 16])
                        && nested_invocation_id != request.request_id
                        && nested_invocation_id != request.parent_invocation_id,
                    "stream resource did not generate a nested invocation identity",
                )?;
                nested_invocation_id
            }
            AuthenticatedServerResourceResult::Failed {
                failure: call_failure,
                ..
            } => {
                return Err(failure(format!(
                    "stream resource unexpectedly failed: {call_failure:?}"
                )));
            }
        };
        let duplicate = kernel
            .dispatch_authenticated_server_resource(&session, &request)
            .await?;
        require(
            duplicate
                == AuthenticatedServerResourceResult::Failed {
                    stream_id: request.stream_id,
                    request_id: request.request_id,
                    failure: CallFailure::InternalFailure,
                },
            "resource request identity was reused after its first dispatch",
        )?;

        let denied_request = ResourceRequest {
            request_id: InvocationId::new(),
            ..request.clone()
        };
        let denied = kernel.replace_security_snapshot(&snapshot(vec![])).await?;
        let denied_session = denied.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let failed = kernel
            .dispatch_authenticated_server_resource(&denied_session, &denied_request)
            .await?;
        require(
            failed
                == AuthenticatedServerResourceResult::Failed {
                    stream_id: denied_request.stream_id,
                    request_id: denied_request.request_id,
                    failure: CallFailure::ExecuteDenied,
                },
            "stream resource without its EXECUTE grant was not denied",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let target_audits = audits
            .iter()
            .filter(|event| {
                event.decision().kind() == SecurityAuditKind::Execute
                    && event.decision().target()
                        == Some(InvocationTarget::new(target, active.pair()))
            })
            .collect::<Vec<_>>();
        let inspect_audits = audits
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Inspect)
            .collect::<Vec<_>>();
        require(
            target_audits.len() == 2
                && target_audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && target_audits[0].decision().effective_principal() == Some(RAW_CLIENT_USER)
                && target_audits[0].decision().authorising_principal() == Some(RAW_CLIENT_USER)
                && target_audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && target_audits[1].decision().denial()
                    == Some(SecurityAuditDenial::Execute(
                        ExecuteDenial::MissingExecuteGrant,
                    ))
                && target_audits[1].decision().effective_principal().is_none()
                && target_audits[1]
                    .decision()
                    .authorising_principal()
                    .is_none()
                && inspect_audits.len() == 1,
            "stream resource audit evidence exposed an unredacted decision",
        )?;
        let audit_text = format!("{audits:?}");
        require(
            !audit_text.contains(&format!("Integer({RESOURCE_VALUE})"))
                && !audit_text.contains(&nested.canonical()),
            "resource audit evidence retained raw argument or result detail",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_direct_resource_post_reservation_failure_is_compensated_once()
-> TestResult<()> {
    const RESOURCE_INPUT: &str = "direct-resource-post-reservation-failure";
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (active, standard_upgrade, _client_function, _server_function) =
            install_raw_client_fixture(&kernel).await?;
        let (active, _resource_client, target, parameter, call_site) =
            install_stream_resource_client_fixture(
                &kernel,
                &active,
                standard_upgrade.checked_standard_library(),
            )
            .await
            .map_err(|error| {
                failure(format!("install stream resource fixture failed: {error:?}"))
            })?;
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        let security = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            function_targets,
            vec![principal],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, target)],
        )?;
        let allowed = kernel.replace_security_snapshot(&security).await?;
        let session = allowed.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let mut request = ResourceRequest {
            stream_id: 203,
            request_id: InvocationId::from_bytes([0xd1; 16]),
            parent_invocation_id: InvocationId::from_bytes([0xd2; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text(RESOURCE_INPUT.into()),
            }],
            item_window: 1,
            byte_window: 1024,
        };
        let parent_request =
            sealed_scalar_resource_request(SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID)?;
        let parent =
            create_authenticated_parent_invocation(&kernel, &active, &session, parent_request)
                .await?;
        request.parent_invocation_id = parent;

        let dispatch = kernel
            .dispatch_authenticated_server_resource_with_forced_post_reservation_failure(
                &session, &request,
            )
            .await;
        require(
            matches!(
                dispatch,
                Err(PostgresKernelError::Database(source))
                    if source.as_db_error().is_some_and(|database| {
                        database.code() == &SqlState::UNDEFINED_COLUMN
                            && database
                                .message()
                                .contains("no_such_direct_resource_post_reservation_column")
                    })
            ),
            "forced direct post-reservation failure did not preserve the injected SQLSTATE",
        )?;
        assert_resource_compensation_audit_row(
            &database,
            &request,
            None,
            "denied",
            "failed",
            "denied",
            RESOURCE_INPUT,
        )
        .await?;

        let audit_session = database.open().await?;
        let audit_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT
                         (SELECT count(*)
                            FROM _orna_kernel.resource_request_history
                           WHERE request_id = $1) AS history_count,
                         (SELECT count(*)
                            FROM _orna_kernel.resource_audit_events
                           WHERE request_id = $1) AS resource_count,
                         (SELECT count(*)
                            FROM _orna_kernel.resource_audit_events AS resource
                            JOIN _orna_kernel.invocation_audit_events AS invocation
                              ON invocation.invocation_id = resource.nested_invocation_id
                           WHERE resource.request_id = $1) AS invocation_count",
                    &[&request.request_id.to_bytes().to_vec()],
                )
                .await?;
            let history_count: i64 = row.try_get("history_count")?;
            let resource_count: i64 = row.try_get("resource_count")?;
            let invocation_count: i64 = row.try_get("invocation_count")?;
            require(
                history_count == 1 && resource_count == 1 && invocation_count == 0,
                "direct post-reservation failure fabricated a nested invocation audit",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "direct resource failure compensation count",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_resource_worker_failure_is_compensated_once() -> TestResult<()> {
    const RESOURCE_INPUT: &str = "resource-worker-input";
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let (
            active,
            standard_upgrade,
            _client_function,
            _server_function,
        ) = install_raw_client_fixture(&kernel).await?;
        let (active, _resource_client, target, parameter, call_site) =
            install_stream_resource_client_fixture(
                &kernel,
                &active,
                standard_upgrade.checked_standard_library(),
            )
            .await
            .map_err(|error| failure(format!("install stream resource fixture failed: {error:?}")))?;
        let mut request = ResourceRequest {
            stream_id: 201,
            request_id: InvocationId::from_bytes([0xa1; 16]),
            parent_invocation_id: InvocationId::from_bytes([0xa2; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text(RESOURCE_INPUT.into()),
            }],
            item_window: 1,
            byte_window: 1024,
        };
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        let security = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            function_targets,
            vec![principal],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, target)],
        )?;
        let active_security = kernel.replace_security_snapshot(&security).await?;
        let session = active_security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let parent_request = sealed_scalar_resource_request(
            SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        )?;
        let parent =
            create_authenticated_parent_invocation(&kernel, &active, &session, parent_request)
                .await?;
        request.parent_invocation_id = parent;
        let cancellation = ResourceCancellation::new();
        let worker_failure = kernel
            .start_authenticated_server_resource_producer_with_forced_pre_acceptance_failure(
                &session,
                &request,
                &cancellation,
            )
            .await;
        require(
            matches!(
                worker_failure,
                Err(PostgresKernelError::Database(source))
                    if source.as_db_error().is_some_and(|database| {
                        database.code() == &SqlState::UNDEFINED_COLUMN
                            && database.message().contains("no_such_resource_producer_column")
                    })
            ),
            "forced producer failure did not preserve the injected undefined-column SQLSTATE",
        )?;

        let request_bytes = request.request_id.to_bytes().to_vec();
        let audit_session = database.open().await?;
        let audit_operation = async {
            let rows = audit_session
                .client()
                .query(
                    "SELECT resource.nested_invocation_id,
                            resource.parent_invocation_id,
                            resource.call_site_id,
                            resource.target_function_id,
                            resource.source_revision_id,
                            resource.catalogue_revision_id,
                            resource.session_principal_id,
                            resource.decision_outcome,
                            resource.terminal_outcome,
                            resource.item_count,
                            resource.byte_count,
                            (SELECT count(*)
                               FROM _orna_kernel.invocation_audit_events AS invocation
                              WHERE invocation.invocation_id = resource.nested_invocation_id)
                                AS invocation_count,
                            row_to_json(resource)::text AS resource_json
                     FROM _orna_kernel.resource_audit_events AS resource
                     WHERE resource.request_id = $1",
                    &[&request_bytes],
                )
                .await?;
            require(rows.len() == 1, "worker failure did not leave exactly one resource audit row")?;
            let row = &rows[0];
            let nested_invocation_id: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
            let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
            let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
            let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision_id: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
            let session_principal_id: Vec<u8> = row.try_get("session_principal_id")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            let invocation_count: i64 = row.try_get("invocation_count")?;
            let resource_json: String = row.try_get("resource_json")?;
            require(
                nested_invocation_id.is_none()
                    && invocation_count == 0
                    && parent_invocation_id == request.parent_invocation_id.to_bytes().to_vec()
                    && call_site_id == request.call_site_id.to_bytes().to_vec()
                    && target_function_id.is_none()
                    && source_revision_id.is_none()
                    && catalogue_revision_id.is_none()
                    && session_principal_id == RAW_CLIENT_USER.to_bytes().to_vec()
                    && decision_outcome == "denied"
                    && terminal_outcome == "failed"
                    && item_count.is_none()
                    && byte_count.is_none()
                    && !resource_json.contains(RESOURCE_INPUT),
                "worker compensation fabricated a nested invocation or exposed target state",
            )?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "worker compensation audit",
        )?;
        let mut post_request = request.clone();
        post_request.stream_id += 1;
        post_request.request_id = InvocationId::from_bytes([0xb1; 16]);
        let post_start = kernel
            .start_authenticated_server_resource_producer_with_forced_post_acceptance_failure(
                &session,
                &post_request,
                &ResourceCancellation::new(),
            )
            .await?;
        let post_producer = match post_start {
            orna_postgres::AuthenticatedServerResourceStart::Accepted(producer) => {
                let pull_result = producer
                    .pull(orna_postgres::ResourceCredit::new(1, 1024).expect("valid test credit"))
                    .await;
                require(
                    pull_result.is_err(),
                    "forced post-acceptance failure did not reach the producer pull",
                )?;
                Some(producer)
            }
            orna_postgres::AuthenticatedServerResourceStart::Failed { .. } => {
                return Err(failure("forced post-acceptance failure did not publish acceptance"));
            }
        };
        let post_request_bytes = post_request.request_id.to_bytes().to_vec();
        let post_audit_session = database.open().await?;
        let post_audit_operation = async {
            let row = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(row) = post_audit_session
                        .client()
                        .query_opt(
                            "SELECT resource.nested_invocation_id,
                                    resource.parent_invocation_id,
                                    resource.call_site_id,
                                    resource.target_function_id,
                                    resource.source_revision_id,
                                    resource.catalogue_revision_id,
                                    resource.decision_outcome,
                                    resource.terminal_outcome,
                                    invocation.outcome AS invocation_outcome,
                                    invocation.function_id AS invocation_function_id,
                                    invocation.source_revision_id AS invocation_source_revision_id,
                                    invocation.catalogue_revision_id AS invocation_catalogue_revision_id,
                                    row_to_json(resource)::text AS resource_json,
                                    row_to_json(invocation)::text AS invocation_json
                             FROM _orna_kernel.resource_audit_events AS resource
                             JOIN _orna_kernel.invocation_audit_events AS invocation
                               ON invocation.invocation_id = resource.nested_invocation_id
                             WHERE resource.request_id = $1",
                            &[&post_request_bytes],
                        )
                        .await?
                    {
                        return Ok::<_, tokio_postgres::Error>(row);
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("post-acceptance worker failure did not leave an audit row"))??;
            let nested_invocation_id: Vec<u8> = row.try_get("nested_invocation_id")?;
            let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
            let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
            let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision_id: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            let invocation_outcome: String = row.try_get("invocation_outcome")?;
            let invocation_function_id: Option<Vec<u8>> = row.try_get("invocation_function_id")?;
            let invocation_source_revision_id: Option<Vec<u8>> =
                row.try_get("invocation_source_revision_id")?;
            let invocation_catalogue_revision_id: Option<Vec<u8>> =
                row.try_get("invocation_catalogue_revision_id")?;
            let resource_json: String = row.try_get("resource_json")?;
            let invocation_json: String = row.try_get("invocation_json")?;
            require(
                nested_invocation_id.len() == 16
                    && nested_invocation_id != post_request.request_id.to_bytes().to_vec()
                    && parent_invocation_id == post_request.parent_invocation_id.to_bytes().to_vec()
                    && call_site_id == post_request.call_site_id.to_bytes().to_vec()
                    && target_function_id == Some(target.to_bytes().to_vec())
                    && source_revision_id == Some(post_request.target_revision.source().to_bytes().to_vec())
                    && catalogue_revision_id
                        == Some(post_request.target_revision.catalogue().to_bytes().to_vec())
                    && decision_outcome == "allowed"
                    && terminal_outcome == "failed"
                    && invocation_outcome == "allowed"
                    && invocation_function_id == Some(target.to_bytes().to_vec())
                    && invocation_source_revision_id
                        == Some(post_request.target_revision.source().to_bytes().to_vec())
                    && invocation_catalogue_revision_id
                        == Some(post_request.target_revision.catalogue().to_bytes().to_vec())
                    && !resource_json.contains(RESOURCE_INPUT)
                    && !invocation_json.contains(RESOURCE_INPUT),
                "post-acceptance worker failure did not preserve bounded allowed identity evidence",
            )?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }
        .await;
        finish_session(
            post_audit_operation,
            post_audit_session.shutdown().await,
            "post-acceptance worker compensation audit",
        )?;
        drop(post_producer);
        let mut post_audit_request = request.clone();
        post_audit_request.stream_id += 2;
        post_audit_request.request_id = InvocationId::from_bytes([0xc1; 16]);
        let post_audit_result = kernel
            .start_authenticated_server_resource_producer_with_forced_post_acceptance_audit_failure(
                &session,
                &post_audit_request,
                &ResourceCancellation::new(),
            )
            .await?;
        match post_audit_result {
            orna_postgres::AuthenticatedServerResourceStart::Failed {
                stream_id,
                request_id,
                failure,
            } => require(
                stream_id == post_audit_request.stream_id
                    && request_id == post_audit_request.request_id
                    && failure == CallFailure::InternalFailure,
                "post-acceptance audit failure did not return its redacted failure",
            )?,
            orna_postgres::AuthenticatedServerResourceStart::Accepted(_) => {
                return Err(failure("post-acceptance audit failure was accepted"));
            }
        }
        assert_resource_compensation_audit_row(
            &database,
            &post_audit_request,
            Some(target),
            "allowed",
            "failed",
            "allowed",
            RESOURCE_INPUT,
        )
        .await?;

        let mut post_cancel_request = request.clone();
        post_cancel_request.stream_id += 3;
        post_cancel_request.request_id = InvocationId::from_bytes([0xc2; 16]);
        let post_cancel = ResourceCancellation::new();
        let post_cancel_result = kernel
            .start_authenticated_server_resource_producer_with_forced_post_acceptance_cancelled_audit_failure(
                &session,
                &post_cancel_request,
                &post_cancel,
            )
            .await?;
        match post_cancel_result {
            orna_postgres::AuthenticatedServerResourceStart::Failed {
                stream_id,
                request_id,
                failure,
            } => require(
                stream_id == post_cancel_request.stream_id
                    && request_id == post_cancel_request.request_id
                    && failure == CallFailure::InternalFailure,
                "cancelled post-acceptance audit failure did not return its redacted failure",
            )?,
            orna_postgres::AuthenticatedServerResourceStart::Accepted(_) => {
                return Err(failure("cancelled post-acceptance audit failure was accepted"));
            }
        }
        require(
            post_cancel.is_requested() && !post_cancel.try_begin_commit(),
            "cancelled post-acceptance audit compensation consumed the cancellation winner",
        )?;
        assert_resource_compensation_audit_row(
            &database,
            &post_cancel_request,
            Some(target),
            "allowed",
            "cancelled",
            "allowed",
            RESOURCE_INPUT,
        )
        .await?;
        let mut cancelled_exit_request = request.clone();
        cancelled_exit_request.stream_id += 4;
        cancelled_exit_request.request_id = InvocationId::from_bytes([0xc4; 16]);
        let cancelled_exit = ResourceCancellation::new();
        let cancelled_exit_result = kernel
            .start_authenticated_server_resource_producer_with_forced_post_acceptance_cancelled_exit_audit_failure(
                &session,
                &cancelled_exit_request,
                &cancelled_exit,
            )
            .await?;
        match cancelled_exit_result {
            orna_postgres::AuthenticatedServerResourceStart::Accepted(producer) => drop(producer),
            orna_postgres::AuthenticatedServerResourceStart::Failed { .. } => {
                return Err(failure("cancelled producer exit audit did not publish acceptance"));
            }
        }
        require(
            cancelled_exit.is_requested() && !cancelled_exit.try_begin_commit(),
            "cancelled producer exit compensation consumed the cancellation winner",
        )?;
        assert_resource_compensation_audit_row(
            &database,
            &cancelled_exit_request,
            Some(target),
            "allowed",
            "cancelled",
            "allowed",
            RESOURCE_INPUT,
        )
        .await?;

        let mut finalizer_cancel_request = request.clone();
        finalizer_cancel_request.stream_id += 5;
        finalizer_cancel_request.request_id = InvocationId::from_bytes([0xc3; 16]);
        let finalizer_cancel = ResourceCancellation::new();
        require(
            finalizer_cancel.request_cancel(),
            "pre-finalizer cancellation did not win the cancellation race",
        )?;
        let finalizer_cancel_result = kernel
            .start_authenticated_server_resource_producer_with_forced_pre_acceptance_failure(
                &session,
                &finalizer_cancel_request,
                &finalizer_cancel,
            )
            .await;
        require(
            finalizer_cancel_result.is_err(),
            "pre-finalizer cancellation unexpectedly returned a public failure value",
        )?;
        require(
            finalizer_cancel.is_requested() && !finalizer_cancel.try_begin_commit(),
            "finalizer cancellation compensation consumed the cancellation winner",
        )?;
        assert_resource_compensation_audit_row(
            &database,
            &finalizer_cancel_request,
            None,
            "denied",
            "cancelled",
            "denied",
            RESOURCE_INPUT,
        )
        .await?;

        let duplicate = kernel
            .start_authenticated_server_resource_producer_with_forced_pre_acceptance_failure(
                &session,
                &request,
                &ResourceCancellation::new(),
            )
            .await?;
        match duplicate {
            orna_postgres::AuthenticatedServerResourceStart::Failed {
                stream_id,
                request_id,
                failure,
            } => require(
                stream_id == request.stream_id
                    && request_id == request.request_id
                    && failure == CallFailure::InternalFailure,
                "reusing a compensated resource request did not return its redacted duplicate failure",
            )?,
            orna_postgres::AuthenticatedServerResourceStart::Accepted(_) => {
                return Err(failure("duplicate resource request was accepted"));
            }
        }
        let count_session = database.open().await?;
        let count_operation = async {
            let row = count_session
                .client()
                .query_one(
                    "SELECT
                         (SELECT count(*)
                            FROM _orna_kernel.resource_audit_events
                           WHERE request_id = $1) AS resource_count,
                         (SELECT count(*)
                            FROM _orna_kernel.resource_audit_events AS resource
                            JOIN _orna_kernel.invocation_audit_events AS invocation
                              ON invocation.invocation_id = resource.nested_invocation_id
                           WHERE resource.request_id = $1) AS invocation_count,
                         (SELECT count(*)
                            FROM _orna_kernel.resource_request_history
                           WHERE request_id = $1) AS history_count",
                    &[&request_bytes],
                )
                .await?;
            let resource_count: i64 = row.try_get("resource_count")?;
            let invocation_count: i64 = row.try_get("invocation_count")?;
            let history_count: i64 = row.try_get("history_count")?;
            require(
                resource_count == 1 && invocation_count == 0 && history_count == 1,
                "duplicate preaccept resource request inserted an invocation or extra audit row",
            )
        }
        .await;
        finish_session(
            count_operation,
            count_session.shutdown().await,
            "worker compensation duplicate count",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

async fn assert_resource_compensation_audit_row(
    database: &TestDatabase,
    request: &ResourceRequest,
    expected_target: Option<FunctionId>,
    expected_decision: &str,
    expected_terminal: &str,
    expected_invocation_outcome: &str,
    raw_marker: &str,
) -> TestResult<()> {
    let request_bytes = request.request_id.to_bytes().to_vec();
    let audit_session = database.open().await?;
    let audit_operation = async {
        let row = timeout(Duration::from_secs(5), async {
            loop {
                if let Some(row) = audit_session
                    .client()
                    .query_opt(
                        "SELECT resource.nested_invocation_id,
                                resource.parent_invocation_id,
                                resource.call_site_id,
                                resource.target_function_id,
                                resource.source_revision_id,
                                resource.catalogue_revision_id,
                                resource.session_principal_id,
                                resource.decision_outcome,
                                resource.terminal_outcome,
                                invocation.outcome AS invocation_outcome,
                                invocation.session_principal_id AS invocation_session_principal_id,
                                invocation.effective_principal_id AS invocation_effective_principal_id,
                                invocation.authorising_principal_id AS invocation_authorising_principal_id,
                                invocation.function_id AS invocation_function_id,
                                invocation.source_revision_id AS invocation_source_revision_id,
                                invocation.catalogue_revision_id AS invocation_catalogue_revision_id,
                                invocation.security_audit_event_id AS invocation_security_audit_event_id,
                                row_to_json(resource)::text AS resource_json,
                                row_to_json(invocation)::text AS invocation_json
                         FROM _orna_kernel.resource_audit_events AS resource
                         LEFT JOIN _orna_kernel.invocation_audit_events AS invocation
                           ON invocation.invocation_id = resource.nested_invocation_id
                         WHERE resource.request_id = $1",
                        &[&request_bytes],
                    )
                    .await?
                {
                    return Ok::<_, tokio_postgres::Error>(row);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| failure("resource compensation did not leave its audit row"))??;
        let nested_invocation_id: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
        let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
        let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
        let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
        let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
        let catalogue_revision_id: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
        let session_principal_id: Vec<u8> = row.try_get("session_principal_id")?;
        let decision_outcome: String = row.try_get("decision_outcome")?;
        let terminal_outcome: String = row.try_get("terminal_outcome")?;
        let invocation_outcome: Option<String> = row.try_get("invocation_outcome")?;
        let invocation_session_principal_id: Option<Vec<u8>> =
            row.try_get("invocation_session_principal_id")?;
        let invocation_effective_principal_id: Option<Vec<u8>> =
            row.try_get("invocation_effective_principal_id")?;
        let invocation_authorising_principal_id: Option<Vec<u8>> =
            row.try_get("invocation_authorising_principal_id")?;
        let invocation_function_id: Option<Vec<u8>> = row.try_get("invocation_function_id")?;
        let invocation_source_revision_id: Option<Vec<u8>> =
            row.try_get("invocation_source_revision_id")?;
        let invocation_catalogue_revision_id: Option<Vec<u8>> =
            row.try_get("invocation_catalogue_revision_id")?;
        let invocation_security_audit_event_id: Option<Vec<u8>> =
            row.try_get("invocation_security_audit_event_id")?;
        let resource_json: String = row.try_get("resource_json")?;
        let invocation_json: Option<String> = row.try_get("invocation_json")?;
        let target_bytes = expected_target.map(|target| target.to_bytes().to_vec());
        let source_bytes = expected_target.map(|_| request.target_revision.source().to_bytes().to_vec());
        let catalogue_bytes =
            expected_target.map(|_| request.target_revision.catalogue().to_bytes().to_vec());
        let principal_bytes = RAW_CLIENT_USER.to_bytes().to_vec();
        let invocation_expected = expected_target.is_some();
        let nested_identity_valid = match (&nested_invocation_id, invocation_expected) {
            (Some(nested), true) => {
                nested.len() == 16
                    && nested.as_slice() != request.request_id.to_bytes().as_slice()
                    && nested.as_slice() != request.parent_invocation_id.to_bytes().as_slice()
            }
            (None, false) => true,
            _ => false,
        };
        require(
            nested_identity_valid
                && parent_invocation_id == request.parent_invocation_id.to_bytes().to_vec()
                && call_site_id == request.call_site_id.to_bytes().to_vec()
                && target_function_id == target_bytes
                && source_revision_id == source_bytes
                && catalogue_revision_id == catalogue_bytes
                && session_principal_id == principal_bytes
                && decision_outcome == expected_decision
                && terminal_outcome == expected_terminal
                && invocation_outcome.as_deref()
                    == expected_target.map(|_| expected_invocation_outcome)
                && invocation_session_principal_id
                    == expected_target.map(|_| principal_bytes.clone())
                && invocation_effective_principal_id
                    == expected_target.map(|_| RAW_CLIENT_USER.to_bytes().to_vec())
                && invocation_authorising_principal_id
                    == expected_target.map(|_| RAW_CLIENT_USER.to_bytes().to_vec())
                && invocation_function_id == target_bytes
                && invocation_source_revision_id == source_bytes
                && invocation_catalogue_revision_id == catalogue_bytes
                && invocation_security_audit_event_id.is_some() == invocation_expected
                && !resource_json.contains(raw_marker)
                && invocation_json.as_deref().is_none_or(|json| !json.contains(raw_marker)),
            "resource compensation changed bounded identity, audit, or redaction evidence",
        )?;
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    finish_session(
        audit_operation,
        audit_session.shutdown().await,
        "resource compensation audit",
    )
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn direct_scalar_resource_holds_active_revision_lock_through_execution() -> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)?;
        let active = kernel.apply_standard_upgrade(&upgrade).await?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("scalar resource lock proof has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source)?;
        let (active, _client, target, call_site) =
            install_scalar_resource_client_fixture(&kernel, &active, &standard).await?;
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.push(SecurityFunctionTarget::verified_standard(
            target,
            standard.verified_snapshot().revision(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        ));
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            function_targets,
            vec![principal],
            vec![],
            vec![ExecuteGrant::new(RAW_CLIENT_USER, target)],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let mut request = ResourceRequest {
            stream_id: 91,
            request_id: InvocationId::from_bytes([0x91; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x92; 16]),
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Single,
            arguments: vec![ResourceArgument {
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
                value: RuntimeValue::Integer(43),
            }],
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        };
        let parent_request = sealed_echo_request(
            InvocationRequestTarget::function_id(target),
            InvocationParameterSelector::parameter_id(STD_INVOKE_ECHO_PARAMETER_ID),
            41,
        )?;
        request.parent_invocation_id =
            create_authenticated_parent_invocation(&kernel, &active, &session, parent_request)
                .await?;

        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("scalar resource lock proof has no source unit"))?;
        let changed_source = SourceBundle::new(active.source().units().iter().enumerate().map(
            |(ordinal, unit)| {
                let content = if ordinal == last_ordinal {
                    format!(
                        "{}\n-- direct scalar resource lock interleave",
                        unit.content()
                    )
                } else {
                    unit.content().to_owned()
                };
                SourceUnit::new(unit.logical_path(), content)
            },
        ))?;
        let changed_report = check_standard_application(
            &changed_source,
            &StandardApplicationCheckContext::try_new(active.catalogue(), &standard)?,
        );
        require(
            changed_report.diagnostics().is_empty(),
            "direct scalar lock interleave source-only apply did not compile",
        )?;
        let changed = prepare_standard_application(&changed_report, active.pair(), &active)?;
        let changed_pair = changed.candidate_pair();

        let reached = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let dispatch_kernel = kernel.clone();
        let dispatch_session = session.clone();
        let dispatch_request = request.clone();
        let dispatch_reached = reached.clone();
        let dispatch_resume = resume.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_kernel
                .dispatch_authenticated_server_resource_with_test_barrier(
                    &dispatch_session,
                    &dispatch_request,
                    dispatch_reached,
                    dispatch_resume,
                )
                .await
        });
        timeout(Duration::from_secs(5), reached.wait())
            .await
            .map_err(|_| {
                failure("direct scalar resource dispatch did not reach validation barrier")
            })?;

        let waiter = database.open().await?;
        let apply_kernel = kernel.clone();
        let mut apply = tokio::spawn(async move { apply_kernel.apply(&changed).await });
        let apply_waiting = timeout(Duration::from_secs(5), async {
            loop {
                if apply.is_finished() {
                    return Ok::<bool, tokio_postgres::Error>(false);
                }
                let waiting = waiter
                    .client()
                    .query_one(
                        "SELECT EXISTS (
                             SELECT 1
                             FROM pg_stat_activity AS waiting
                            WHERE waiting.pid <> pg_backend_pid()
                              AND waiting.wait_event_type = 'Lock'
                              AND waiting.query LIKE '%_orna_kernel.active_revision%'
                              AND cardinality(pg_blocking_pids(waiting.pid)) > 0
                         )",
                        &[],
                    )
                    .await?
                    .get::<_, bool>(0);
                if waiting {
                    return Ok(true);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| failure("apply did not reach the active-revision lock wait"))??;

        resume.wait().await;
        let dispatched = timeout(Duration::from_secs(5), dispatch)
            .await
            .map_err(|_| failure("direct scalar resource dispatch did not resume"))?;
        let dispatched = dispatched
            .map_err(|error| failure(format!("direct scalar resource task failed: {error}")))?;
        let dispatched = dispatched?;
        let applied = timeout(Duration::from_secs(5), &mut apply)
            .await
            .map_err(|_| failure("source-only apply did not resume after resource completion"))?;
        let applied =
            applied.map_err(|error| failure(format!("source-only apply task failed: {error}")))?;
        let applied = applied?;
        waiter.shutdown().await?;

        require(
            apply_waiting,
            "source-only apply committed while direct scalar resource execution was paused",
        )?;
        require(
            applied.pair() == changed_pair,
            "source-only apply did not commit its replacement active revision",
        )?;
        match dispatched {
            AuthenticatedServerResourceResult::Completed {
                target_revision,
                resource_kind,
                values,
                ..
            } => {
                require(
                    target_revision == active.pair()
                        && resource_kind == ResourceKind::Single
                        && values == [RuntimeValue::Integer(43)],
                    "direct scalar resource did not execute against its locked active revision",
                )?;
            }
            AuthenticatedServerResourceResult::Failed {
                failure: call_failure,
                ..
            } => {
                return Err(failure(format!(
                    "direct scalar resource unexpectedly failed: {call_failure:?}"
                )));
            }
        }
        require(
            applied.pair() != active.pair(),
            "source-only apply did not advance the active revision pair",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn installed_executor_reclaims_transport_after_terminal_cancellation() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("installed-executor-cancellation-live".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "build installed executor cancellation runtime failed: {error}"
                    ))
                })?;
            runtime
                .block_on(installed_executor_reclaims_transport_after_terminal_cancellation_inner())
        })
        .map_err(|error| {
            failure(format!(
                "spawn installed executor cancellation thread failed: {error}"
            ))
        })?;
    handle
        .join()
        .map_err(|_| failure("installed executor cancellation thread panicked"))?
}

#[cfg(feature = "test-hooks")]
async fn installed_executor_reclaims_transport_after_terminal_cancellation_inner() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = open_standard_database(kernel(&database)?)
            .await
            .map_err(|error| failure(format!("open standard database failed: {error:?}")))?;
        let active = kernel
            .recover()
            .await
            .map_err(|error| failure(format!("recover installed standard failed: {error:?}")))?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| {
                failure("executor cancellation fixture has no checked standard source")
            })?;
        let checked_standard =
            check_standard_library_source(&standard_source).map_err(|error| {
                failure(format!("installed standard source check failed: {error:?}"))
            })?;
        let (active, _client, target, parameter, call_site) =
            install_stream_resource_client_fixture(&kernel, &active, &checked_standard).await?;
        let expected_type = match active
            .catalogue()
            .function_by_id(target)
            .ok_or_else(|| failure("executor cancellation fixture target is missing"))?
            .return_type()
        {
            FunctionReturn::Stream(expected_type) => *expected_type,
            _ => {
                return Err(failure(
                    "executor cancellation fixture target is not a stream",
                ));
            }
        };
        let arguments = vec![FunctionArgument::new(
            parameter,
            RuntimeValue::Text("resource-value".to_owned()),
        )?];
        let key = ClientResourceKey::new(
            InvocationTarget::new(target, active.pair()),
            RAW_CLIENT_USER,
            ClientResourceKey::canonical_arguments_digest(&active, &arguments)?,
            Sha256Digest::from_bytes([0; 32]),
        );
        let mut resource = ClientResource::new_stream(key, expected_type);
        let context = ClientResourceInvocationContext::new(
            InvocationId::from_bytes([0x91; 16]),
            call_site,
            String::new(),
            String::new(),
        );
        let first = resource.begin_stream_request_with_context(
            &active,
            context.clone(),
            arguments.clone(),
        )?;
        let replacement =
            resource.begin_stream_request_with_context(&active, context, arguments)?;
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            vec![],
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
            vec![],
        )?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let registry = registered_opaque_codecs(&standard_source)?;
        let (server, client) = StandardUnixStream::pair()?;
        server.set_nonblocking(true)?;
        client.set_nonblocking(true)?;
        let server_active = active.clone();
        let server_registry = registry.clone();
        let (accepted_sender, accepted_receiver) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            let mut server = UnixStream::from_std(server)?;
            let mut hello = [0_u8; 12];
            server
                .read_exact(&mut hello)
                .await
                .map_err(|error| failure(format!("handshake read failed: {error}")))?;
            require(
                hello == *b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00",
                "executor cancellation fixture received an invalid handshake",
            )?;
            server
                .write_all(b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let ResourceClientFrame::Request(first_wire) = read_resource_client_frame_from_socket(
                &mut server,
                &server_active,
                &server_registry,
            )
            .await
            .map_err(|error| failure(format!("first request read failed: {error}")))?
            else {
                return Err(failure(
                    "executor cancellation fixture did not receive its first request",
                ));
            };
            require(
                first_wire.stream_id == 1,
                "first executor resource did not use stream 1",
            )?;
            send_resource_server_frame_to_socket(
                &mut server,
                &server_active,
                &server_registry,
                &ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
                    stream_id: first_wire.stream_id,
                    request_id: first_wire.request_id,
                    nested_invocation_id: InvocationId::from_bytes([0xa1; 16]),
                    target_revision: first_wire.target_revision,
                    resource_kind: ResourceKind::Stream,
                }),
            )
            .await?;
            accepted_sender
                .send(())
                .map_err(|_| failure("executor cancellation fixture lost acceptance waiter"))?;
            let ResourceClientFrame::Cancel(cancel) = read_resource_client_frame_from_socket(
                &mut server,
                &server_active,
                &server_registry,
            )
            .await
            .map_err(|error| failure(format!("cancel read failed: {error}")))?
            else {
                return Err(failure(
                    "executor cancellation fixture did not receive cancel",
                ));
            };
            require(
                cancel.stream_id == first_wire.stream_id
                    && cancel.request_id == first_wire.request_id
                    && cancel.reason == ResourceCancellationCode::ClientRequested,
                "executor cancellation fixture received an incorrect cancel frame",
            )?;
            send_resource_server_frame_to_socket(
                &mut server,
                &server_active,
                &server_registry,
                &ResourceServerFrame::Cancelled(orna_protocol::ResourceCancelled {
                    stream_id: cancel.stream_id,
                    request_id: cancel.request_id,
                    target_revision: first_wire.target_revision,
                    reason: ResourceCancellationCode::ClientRequested,
                }),
            )
            .await?;
            let ResourceClientFrame::Request(replacement_wire) =
                read_resource_client_frame_from_socket(
                    &mut server,
                    &server_active,
                    &server_registry,
                )
                .await
                .map_err(|error| failure(format!("replacement request read failed: {error}")))?
            else {
                return Err(failure(
                    "executor cancellation fixture did not receive replacement request",
                ));
            };
            require(
                replacement_wire.stream_id == 2,
                "replacement executor resource did not wait for terminal cancellation reclamation",
            )?;
            send_resource_server_frame_to_socket(
                &mut server,
                &server_active,
                &server_registry,
                &ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
                    stream_id: replacement_wire.stream_id,
                    request_id: replacement_wire.request_id,
                    nested_invocation_id: InvocationId::from_bytes([0xa2; 16]),
                    target_revision: replacement_wire.target_revision,
                    resource_kind: ResourceKind::Stream,
                }),
            )
            .await?;
            send_resource_server_frame_to_socket(
                &mut server,
                &server_active,
                &server_registry,
                &ResourceServerFrame::Completed(orna_protocol::ResourceCompleted {
                    stream_id: replacement_wire.stream_id,
                    request_id: replacement_wire.request_id,
                    target_revision: replacement_wire.target_revision,
                    final_batch_sequence: 0,
                    total_items: 0,
                }),
            )
            .await?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        });
        let mut executor =
            InstalledClientResourceExecutor::new_with_stream(kernel, session, active, client);
        require(
            matches!(
                executor.execute(first.clone()),
                ClientResourceCompletion::Pending { .. }
            ),
            "first executor resource did not become pending",
        )?;
        tokio::time::timeout(Duration::from_secs(5), accepted_receiver)
            .await
            .map_err(|_| failure("executor cancellation fixture did not reach acceptance"))?
            .map_err(|_| failure("executor cancellation fixture acceptance waiter failed"))?;
        let (mut executor, cancelled) = tokio::task::spawn_blocking(move || {
            let cancelled = executor.cancel(first);
            (executor, cancelled)
        })
        .await
        .map_err(|error| failure(format!("executor cancellation task failed: {error}")))?;
        require(
            matches!(cancelled, ClientResourceCompletion::Cancelled { .. }),
            "terminal resource cancellation did not complete as cancelled",
        )?;
        require(
            matches!(
                executor.execute(replacement),
                ClientResourceCompletion::Pending { .. }
            ),
            "executor rejected a replacement after terminal cancellation",
        )?;
        let server_result = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .map_err(|_| failure("executor cancellation server timed out"))?
            .map_err(|error| failure(format!("executor cancellation server failed: {error}")))?;
        server_result?;
        let replacement_completion = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(completion) = executor.poll() {
                    return completion;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| failure("replacement polling timed out"))?;
        require(
            matches!(
                replacement_completion,
                ClientResourceCompletion::StreamCompleted { .. }
            ),
            "replacement resource did not complete after terminal cancellation reclaimed transport",
        )
    })
    .await
}
