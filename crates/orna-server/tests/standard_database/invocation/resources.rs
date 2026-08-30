use super::*;

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_scalar_client_resource_pending_continues_through_installed_evaluator()
-> TestResult<()> {
    const CONNECTION_PROTOCOL_MAJOR: u16 = 5;

    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel
            .bootstrap()
            .await
            .map_err(|error| failure(format!("bootstrap failed: {error:?}")))?;
        let empty = kernel
            .recover()
            .await
            .map_err(|error| failure(format!("recover empty database failed: {error:?}")))?;
        let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)
            .map_err(|error| failure(format!("prepare V1-to-V2 upgrade failed: {error:?}")))?;
        let active = kernel
            .apply_standard_upgrade(&upgrade)
            .await
            .map_err(|error| failure(format!("apply V1-to-V2 upgrade failed: {error:?}")))?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("scalar resource fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!("installed standard source check failed: {error:?}"))
        })?;
        let (active, client, target, _call_site) =
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
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            function_targets,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client),
                ExecuteGrant::new(RAW_CLIENT_USER, target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("scalar resource proof has no standard context"))?;
        let registry = registered_opaque_codecs(standard)?;
        let request = sealed_scalar_resource_request(client)?;
        let retained = encode_invoke_request(&active, &registry, &request)?;
        let (server, client_stream) = StandardUnixStream::pair()?;
        client_stream.set_nonblocking(true)?;
        let authorizer = RawResourceRequestAuthorizer::new();
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer.clone(),
        ));
        let mut executor = RecordingInstalledResourceExecutor::new(
            kernel.clone(),
            session.clone(),
            active.clone(),
            client_stream,
            authorizer,
        );
        let dispatch = kernel
            .dispatch_sealed_sys_invoke_with_resource_executor(
                &session,
                CONNECTION_PROTOCOL_MAJOR,
                &retained,
                Some(&mut executor),
            )
            .await
            .map_err(|error| failure(format!("scalar pending dispatch failed: {error:?}")));
        let execute_count = executor.execute_count;
        let poll_count = executor.poll_count;
        drop(executor);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let dispatch = finish_session(dispatch, connection, "scalar pending socket cleanup")?;
        let SealedInvocationResult::Completed { events, .. } = dispatch else {
            return Err(failure(
                "scalar pending resource did not complete the sealed invocation",
            ));
        };
        let records = events.records();
        require(
            records.len() == 3
                && records[0].event().kind() == InvocationEventKind::InvocationStarted
                && records[1].event().kind() == InvocationEventKind::ValueBatch
                && records[2].event().kind() == InvocationEventKind::InvocationCompleted,
            "scalar pending resource did not retain the completed invocation event sequence",
        )?;
        let InvocationEventBody::ValueBatch {
            schema: None,
            values,
        } = records[1].event().body()
        else {
            return Err(failure(
                "scalar pending resource completion did not carry a plain typed batch",
            ));
        };
        require(
            values.len() == 1 && values[0].value() == &RuntimeValue::Integer(43),
            "scalar pending resource completion was not typed INTEGER",
        )?;
        require(
            execute_count == 1 && poll_count > 0,
            "scalar pending resource did not execute once and continue through poll",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_installed_no_broker_resource_returns_typed_result_and_terminal() -> TestResult<()> {
    const CONNECTION_PROTOCOL_MAJOR: u16 = 5;

    with_test_database(|database| async move {
        let uid = nix::unistd::getuid().as_raw();
        let kernel = kernel(&database)?;
        kernel
            .bootstrap()
            .await
            .map_err(|error| failure(format!("bootstrap failed: {error:?}")))?;
        let empty = kernel
            .recover()
            .await
            .map_err(|error| failure(format!("recover empty database failed: {error:?}")))?;
        let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)
            .map_err(|error| failure(format!("prepare V1-to-V2 upgrade failed: {error:?}")))?;
        let active = kernel
            .apply_standard_upgrade(&upgrade)
            .await
            .map_err(|error| failure(format!("apply V1-to-V2 upgrade failed: {error:?}")))?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("no-broker resource fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source)
            .map_err(|error| failure(format!("installed standard source check failed: {error:?}")))?;
        let (active, client_function, target, _call_site) =
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
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            function_targets,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client_function),
                ExecuteGrant::new(RAW_CLIENT_USER, target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("no-broker resource proof has no standard context"))?;
        let registry = registered_opaque_codecs(standard)?;
        let request = InvokeRequest::new(InvokeRequestInput {
            target: InvocationRequestTarget::function_id(client_function),
            arguments: Vec::new(),
            caller_context: InvocationCallerContext::new(
                InvocationCallerKind::TestRunner,
                false,
                false,
                None,
                None,
                "en-GB",
                "UTC",
                None,
            )?,
            client_offer: InvocationClientOffer::new(
                CONNECTION_PROTOCOL_MAJOR,
                "en-GB",
                "UTC",
                Vec::new(),
                Vec::new(),
                1_024,
                0,
                None,
                None,
            )?,
            output_requirement: None,
            state_profile: None,
            trace_policy: InvocationTracePolicy::Off,
            idempotency_key: None,
            parent_invocation_id: None,
            observer_context: None,
        })?;
        let retained = encode_invoke_request(&active, &registry, &request)?;

        let (server, client_stream) = StandardUnixStream::pair()?;
        client_stream.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client_stream)?;
        let connection = tokio::spawn(serve_local_raw_stream(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
        ));
        let operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "no-broker resource socket did not complete its constructed handshake",
            )?;
            for frame in [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: SYS_INVOKE_FUNCTION_ID,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1_024,
                },
                ClientFrame::CallInvokeRequest {
                    stream: 1,
                    request: retained.clone(),
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ] {
                send_constructed_protocol_frame(&mut client, &active, &registry, &frame).await?;
            }
            require(
                matches!(
                    read_constructed_invocation_protocol_frame(&mut client, &active, &registry).await?,
                    ServerFrame::CallAccepted { stream: 1, .. }
                ),
                "no-broker resource socket did not accept the sealed invocation",
            )?;
            let started =
                read_constructed_invocation_protocol_frame(&mut client, &active, &registry).await?;
            require(
                matches!(
                    &started,
                    ServerFrame::EventBatch { stream: 1, channel: Channel::ResultValues, events }
                        if events.len() == 1
                            && matches!(
                                &events[0].event,
                                Event::Value(RuntimeValue::InvokeEvent(event))
                                    if event.kind() == InvocationEventKind::InvocationStarted
                            )
                ),
                "no-broker resource socket did not publish invocation start",
            )?;
            let terminal_events =
                read_constructed_invocation_protocol_frame(&mut client, &active, &registry).await?;
            require(
                matches!(
                    &terminal_events,
                    ServerFrame::EventBatch { stream: 1, channel: Channel::ResultValues, events }
                        if events.len() == 2
                            && matches!(
                                &events[0].event,
                                Event::Value(RuntimeValue::InvokeEvent(event))
                                    if event.kind() == InvocationEventKind::ValueBatch
                                        && matches!(
                                            event.body(),
                                            InvocationEventBody::ValueBatch { schema: None, values }
                                                if values.len() == 1
                                                    && values[0].value() == &RuntimeValue::Integer(43)
                                        )
                            )
                            && matches!(
                                &events[1].event,
                                Event::Value(RuntimeValue::InvokeEvent(event))
                                    if event.kind() == InvocationEventKind::InvocationCompleted
                            )
                ),
                "no-broker resource socket did not return its typed scalar and terminal event",
            )?;
            require(
                read_constructed_invocation_protocol_frame(&mut client, &active, &registry).await?
                    == ServerFrame::CallCompleted { stream: 1 },
                "no-broker resource socket did not close the sealed invocation terminally",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            operation,
            finish_session(shutdown, connection, "no-broker resource socket cleanup"),
            "no-broker resource socket operation",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn proves_procedural_client_resource_through_installed_evaluator() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("procedural-client-resource-live".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "build procedural client resource runtime failed: {error}"
                    ))
                })?;
            runtime.block_on(proves_procedural_client_resource_through_installed_evaluator_inner())
        })
        .map_err(|error| {
            failure(format!(
                "spawn procedural client resource thread failed: {error}"
            ))
        })?;
    handle
        .join()
        .map_err(|_| failure("procedural client resource thread panicked"))?
}

async fn proves_procedural_client_resource_through_installed_evaluator_inner() -> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
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
            .ok_or_else(|| failure("procedural resource fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source)
            .map_err(|error| failure(format!("installed standard source check failed: {error:?}")))?;
        let (active, client, target, parameter) =
            install_procedural_resource_client_fixture(&kernel, &active, &standard).await?;
        let host = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["procedural_fixture", "host"])
            .ok_or_else(|| failure("procedural resource fixture is missing its host CLIENT function"))?
            .id();
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["procedural_fixture", "create"])
            .ok_or_else(|| failure("procedural resource fixture is missing its create function"))?
            .id();
        let create_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("procedural resource create disappeared from the catalogue"))?
            .parameter_by_name("p_marker")
            .ok_or_else(|| failure("procedural resource create has no p_marker parameter"))?
            .id();
        kernel
            .execute_server_insert(
                create,
                &[FunctionArgument::new(
                    create_parameter,
                    RuntimeValue::Text("installed-marker".to_owned()),
                )?],
            )
            .await
            .map_err(|error| failure(format!("insert procedural resource fixture row failed: {error:?}")))?;
        let mut functions = active
            .catalogue()
            .functions()
            .iter()
            .map(FunctionDefinition::id)
            .collect::<Vec<_>>();
        if let Some(standard) = active.catalogue_hash_context().standard() {
            functions.extend(
                standard
                    .catalogue()
                    .functions()
                    .iter()
                    .map(FunctionDefinition::id),
            );
        }
        functions.sort_unstable();
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client),
                ExecuteGrant::new(RAW_CLIENT_USER, target),
                ExecuteGrant::new(RAW_CLIENT_USER, host),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(client, active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "installed procedural CLIENT grant was denied: {denial:?}"
                )))
            }
        };
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("installed-marker".to_owned()),
        )?;
        let mut executor = DeterministicStreamResourceExecutor;
        let result = evaluate_client_function_with_arguments_and_executor(
            &active,
            &authorisation,
            std::slice::from_ref(&argument),
            &mut executor,
        )?;
        let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        ))?;
        let list = RuntimeValue::list(
            &active,
            list_descriptor.clone(),
            vec![
                RuntimeValue::Text("stream-one".to_owned()),
                RuntimeValue::Text("stream-two".to_owned()),
            ],
        )?;
        let expected_value = RuntimeValue::option(
            &active,
            TypeDescriptor::option(list_descriptor)?,
            Some(list),
        )?;
        require(
            result.value() == &expected_value,
            "procedural CLIENT LET/AWAIT did not return the expected typed stream value",
        )?;
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("procedural resource proof has no standard context"))?;
        let registry = registered_opaque_codecs(standard)?;
        let host_list_descriptor = TypeDescriptor::list(TypeDescriptor::named(
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        ))?;
        let host_list = RuntimeValue::list(
            &active,
            host_list_descriptor.clone(),
            vec![RuntimeValue::Text("installed-marker".to_owned())],
        )?;
        let host_value = RuntimeValue::option(
            &active,
            TypeDescriptor::option(host_list_descriptor)?,
            Some(host_list),
        )?;
        let mut expected = encode_constructed_value(&active, &registry, &host_value)?;
        expected.push(b'\n');
        let host_revision = active
            .function_revisions()
            .iter()
            .find(|revision| revision.function() == host)
            .ok_or_else(|| failure("procedural resource host is missing its function revision"))?;
        let host_plan = ProceduralClientPlan::decode(host_revision.artifact().payload())?;
        let ClientExpressionNode::Resource { operation } = host_plan.statements()[0].expression() else {
            return Err(failure("procedural resource host did not retain a resource operation"));
        };
        let ClientExpressionNode::Await { expression } = host_plan.return_expression() else {
            return Err(failure("procedural resource host did not retain AWAIT"));
        };
        let ClientExpressionNode::LocalRead { local } = expression.as_ref() else {
            return Err(failure("procedural resource host AWAIT did not retain the resource local read"));
        };
        require(
            *local == host_plan.locals()[0].local_id(),
            "procedural resource host AWAIT did not retain its resource local",
        )?;
        let host_call_site = operation.call_site_id();
        let invoke_target = InvocationRequestTarget::qualified_name(QualifiedSemanticName::new([
            "procedural_fixture",
            "host",
        ])?)?;
        let (outcome, stdout, stderr) = installed_invoke_run(
            &database,
            installed_invoke_request(invoke_target, vec![], true, false),
        )
        .await?;
        require(
            outcome == Ok(InstalledInvokeOutcome::Completed)
                && stdout == expected
                && stderr.is_empty(),
            "installed invoke did not execute the SERVER resource through its host executor",
        )?;
        let target_bytes = target.to_bytes().to_vec();
        let host_bytes = host.to_bytes().to_vec();
        let audit_session = database.open().await?;
        let audit_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT invocation.invocation_id, resource.parent_invocation_id,
                            resource.request_id, resource.call_site_id,
                            invocation.outcome, resource.decision_outcome,
                            resource.terminal_outcome
                     FROM _orna_kernel.resource_audit_events AS resource
                     JOIN _orna_kernel.invocation_audit_events AS invocation
                       ON invocation.invocation_id = resource.parent_invocation_id
                     WHERE resource.target_function_id = $1
                       AND invocation.function_id = $2
                     ORDER BY resource.sequence DESC
                     LIMIT 1",
                    &[&target_bytes, &host_bytes],
                )
                .await?;
            let root_invocation_id: Vec<u8> = row.try_get("invocation_id")?;
            let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
            let request_id: Vec<u8> = row.try_get("request_id")?;
            let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
            let invocation_outcome: String = row.try_get("outcome")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            require(
                root_invocation_id.len() == 16
                    && parent_invocation_id == root_invocation_id
                    && request_id.len() == 16
                    && request_id != root_invocation_id
                    && call_site_id == host_call_site.to_bytes().to_vec(),
                "installed resource audit lost the exact root invocation or compiled call-site identity",
            )?;
            require(
                invocation_outcome == "allowed"
                    && decision_outcome == "allowed"
                    && terminal_outcome == "completed",
                "installed root/resource sequence did not retain allowed terminal audit evidence",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "installed resource identity audit",
        )?;

        let unavailable = evaluate_client_function_with_arguments(
            &active,
            &authorisation,
            std::slice::from_ref(&argument),
        );
        require(
            matches!(
                unavailable,
                Err(ClientExecutionError::ResourceEvaluation {
                    source: orna_client::ClientResourceExecutionError::ExecutorUnavailable,
                    ..
                })
            ),
            "procedural resource without a caller-owned executor did not fail closed",
        )?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn installed_stream_resource_evaluator_consumes_batch_then_returns_terminal_none() -> TestResult<()>
{
    let handle = std::thread::Builder::new()
        .name("orna-installed-stream-evaluator".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!("build stream evaluator runtime failed: {error}"))
                })?;
            runtime.block_on(
                installed_stream_resource_evaluator_consumes_batch_then_returns_terminal_none_inner(
                ),
            )
        })
        .map_err(|error| failure(format!("spawn stream evaluator thread failed: {error}")))?;
    handle
        .join()
        .map_err(|_| failure("stream evaluator thread panicked"))?
}

#[cfg(feature = "test-hooks")]
async fn installed_stream_resource_evaluator_consumes_batch_then_returns_terminal_none_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = open_standard_database(kernel(&database)?).await.map_err(|error| {
            failure(format!("open standard database failed: {error:?}"))
        })?;
        let active = kernel.recover().await.map_err(|error| {
            failure(format!("recover installed standard failed: {error:?}"))
        })?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("stream evaluator fixture has no checked standard source"))?;
        let checked_standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!("installed standard source check failed: {error:?}"))
        })?;
        let (active, _client, _target, _parameter, _call_site) =
            install_stream_resource_client_fixture(&kernel, &active, &checked_standard).await?;
        let client = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "call_all"])
            .ok_or_else(|| failure("stream evaluator fixture is missing resource_fixture.call_all"))?
            .id();
        let target = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "all"])
            .ok_or_else(|| failure("stream evaluator fixture is missing resource_fixture.all"))?
            .id();
        let call_site = {
            let revision = active
                .function_revisions()
                .iter()
                .find(|revision| revision.function() == client)
                .ok_or_else(|| failure("stream evaluator call_all revision is missing"))?;
            let plan = ResourceClientPlan::decode(revision.artifact().payload())?;
            let ClientExpressionNode::Await { expression } = plan.expression() else {
                return Err(failure("stream evaluator call_all is not an awaited resource"));
            };
            let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
                return Err(failure("stream evaluator call_all is not a resource operation"));
            };
            operation.call_site_id()
        };
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "create"])
            .ok_or_else(|| failure("stream evaluator fixture is missing resource_fixture.create"))?
            .id();
        let root = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "root"])
            .ok_or_else(|| failure("stream evaluator fixture is missing resource_fixture.root"))?
            .id();
        let create_definition = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("stream evaluator create disappeared from the catalogue"))?;
        let create_marker = create_definition
            .parameter_by_name("p_marker")
            .ok_or_else(|| failure("stream evaluator create has no p_marker parameter"))?
            .id();
        let create_sequence = create_definition
            .parameter_by_name("p_sequence")
            .ok_or_else(|| failure("stream evaluator create has no p_sequence parameter"))?
            .id();
        kernel
            .execute_server_insert(
                create,
                &[
                    FunctionArgument::new(
                        create_marker,
                        RuntimeValue::Text("evaluator-terminal".to_owned()),
                    )?,
                    FunctionArgument::new(create_sequence, RuntimeValue::Integer(1))?,
                ],
            )
            .await
            .map_err(|error| failure(format!("insert stream evaluator fixture row failed: {error:?}")))?;

        kernel
            .execute_server_insert(
                create,
                &[
                    FunctionArgument::new(
                        create_marker,
                        RuntimeValue::Text("evaluator-terminal-next".to_owned()),
                    )?,
                    FunctionArgument::new(create_sequence, RuntimeValue::Integer(2))?,
                ],
            )
            .await
            .map_err(|error| {
                failure(format!("insert second stream evaluator fixture row failed: {error:?}"))
            })?;
        let mut functions = active
            .catalogue()
            .functions()
            .iter()
            .map(FunctionDefinition::id)
            .collect::<Vec<_>>();
        if let Some(standard) = active.catalogue_hash_context().standard() {
            functions.extend(
                standard
                    .catalogue()
                    .functions()
                    .iter()
                    .map(FunctionDefinition::id),
            );
        }
        functions.sort_unstable();
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, client),
                ExecuteGrant::new(RAW_CLIENT_USER, target),
                ExecuteGrant::new(RAW_CLIENT_USER, root),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let (root_outcome, _, _) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(root),
                vec![],
                true,
                false,
            ),
        )
        .await?;
        require(
            root_outcome == Ok(InstalledInvokeOutcome::Completed),
            "stream evaluator could not create an owned root invocation",
        )?;
        let audit_session = database.open().await?;
        let root_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT invocation_id
                     FROM _orna_kernel.invocation_audit_events
                     WHERE function_id = $1
                       AND session_principal_id = $2
                     ORDER BY sequence DESC
                     LIMIT 1",
                    &[&root.to_bytes().to_vec(), &RAW_CLIENT_USER.to_bytes().to_vec()],
                )
                .await?;
            let bytes: Vec<u8> = row.try_get(0)?;
            let bytes: [u8; 16] = bytes
                .try_into()
                .map_err(|_| failure("root invocation audit identity was not 16 bytes"))?;
            Ok(InvocationId::from_bytes(bytes))
        }
        .await;
        let parent_invocation = finish_session(
            root_operation,
            audit_session.shutdown().await,
            "root invocation audit lookup",
        )?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let authorisation = match security
            .authorise_execute(&session, InvocationTarget::new(client, active.pair()))
        {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "stream evaluator CLIENT grant was denied: {denial:?}"
                )))
            }
        };
        let grants = LocalCapabilityGrantSet::new();
        let mut executor =
            InstalledClientResourceExecutor::new(kernel.clone(), session, active.clone());
        let mut state = ClientStateStore::default();
        let operation = async {
            let pending = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                &[],
                &[],
                &grants,
                &mut state,
                parent_invocation,
                &mut executor,
            )
            .expect_err("the first stream AWAIT unexpectedly completed synchronously");
            let (resource_key, resource_generation) = match pending {
                ClientExecutionError::ResourceEvaluation {
                    source: orna_client::ClientResourceExecutionError::Pending { key, generation },
                    ..
                } => (key, generation),
                error => {
                    return Err(failure(format!(
                        "the first stream AWAIT returned an unexpected result: {error:?}"
                    )))
                }
            };
            let first_completion = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(completion) = executor.poll() {
                        return Ok::<ClientResourceCompletion, Box<dyn Error + Send + Sync>>(completion);
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("installed stream evaluator first batch timed out"))??;
            require(
                matches!(
                    &first_completion,
                    ClientResourceCompletion::StreamValues {
                        key,
                        generation,
                        values,
                        ..
                    } if *key == resource_key
                        && *generation == resource_generation
                        && values.as_slice() == [RuntimeValue::Text("evaluator-terminal".to_owned())]
                ),
                "installed evaluator did not receive its exact typed stream batch",
            )?;
            state
                .resource_mut(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its pending resource"))?
                .apply_completion(&active, first_completion)
                .map_err(|error| failure(format!("apply first stream batch failed: {error:?}")))?;
            let after_batch = state
                .resource(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its first batch"))?;
            require(
                after_batch.status() == ClientResourceStatus::Loading
                    && !after_batch.stream_complete()
                    && after_batch.request_id().is_some(),
                "first stream batch did not retain the loading resource state",
            )?;

            let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            ))?;
            let list = RuntimeValue::list(
                &active,
                list_descriptor.clone(),
                vec![RuntimeValue::Text("evaluator-terminal".to_owned())],
            )?;
            let expected_batch = RuntimeValue::option(
                &active,
                TypeDescriptor::option(list_descriptor)?,
                Some(list),
            )?;
            let batch_result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                &[],
                &[],
                &grants,
                &mut state,
                parent_invocation,
                &mut executor,
            )?;
            require(
                batch_result.value() == &expected_batch,
                "next installed stream AWAIT did not return its typed OPTION<LIST<T>> batch",
            )?;
            let second_completion = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(completion) = executor.poll() {
                        return Ok::<ClientResourceCompletion, Box<dyn Error + Send + Sync>>(
                            completion,
                        );
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("installed stream evaluator second batch timed out"))??;
            require(
                matches!(
                    &second_completion,
                    ClientResourceCompletion::StreamValues {
                        key,
                        generation,
                        values,
                        ..
                    } if *key == resource_key
                        && *generation == resource_generation
                        && values.as_slice()
                            == [RuntimeValue::Text("evaluator-terminal-next".to_owned())]
                ),
                "installed evaluator did not receive its second typed stream batch",
            )?;
            state
                .resource_mut(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its second batch"))?
                .apply_completion(&active, second_completion)
                .map_err(|error| failure(format!("apply second stream batch failed: {error:?}")))?;
            let second_list_descriptor = TypeDescriptor::list(TypeDescriptor::named(
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            ))?;
            let second_list = RuntimeValue::list(
                &active,
                second_list_descriptor.clone(),
                vec![RuntimeValue::Text("evaluator-terminal-next".to_owned())],
            )?;
            let expected_second_batch = RuntimeValue::option(
                &active,
                TypeDescriptor::option(second_list_descriptor)?,
                Some(second_list),
            )?;
            let second_batch_result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                &[],
                &[],
                &grants,
                &mut state,
                parent_invocation,
                &mut executor,
            )?;
            require(
                second_batch_result.value() == &expected_second_batch,
                "installed stream AWAIT did not return its second typed batch",
            )?;

            let terminal_completion = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(completion) = executor.poll() {
                        return Ok::<ClientResourceCompletion, Box<dyn Error + Send + Sync>>(completion);
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("installed stream evaluator terminal completion timed out"))??;
            require(
                matches!(
                    &terminal_completion,
                    ClientResourceCompletion::StreamCompleted {
                        key,
                        generation,
                        ..
                    } if *key == resource_key && *generation == resource_generation
                ),
                "installed evaluator did not receive successful stream terminal completion",
            )?;
            state
                .resource_mut(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its terminal resource"))?
                .apply_completion(&active, terminal_completion)
                .map_err(|error| failure(format!("apply stream terminal completion failed: {error:?}")))?;
            let terminal_state = state
                .resource(resource_key)
                .ok_or_else(|| failure("stream evaluator state lost its completed resource"))?
                .clone();
            require(
                terminal_state.status() == ClientResourceStatus::Ready
                    && terminal_state.stream_complete()
                    && terminal_state.value().is_none()
                    && terminal_state.request_id().is_some(),
                "stream terminal completion did not publish the READY terminal state",
            )?;

            let request_id = terminal_state
                .request_id()
                .ok_or_else(|| failure("completed stream resource lost its request identity"))?;
            let request_id_bytes = request_id.to_bytes().to_vec();
            let audit_session = database.open().await?;
            let audit_operation = async {
                let row = audit_session
                    .client()
                    .query_one(
                        "SELECT resource.parent_invocation_id,
                                resource.call_site_id,
                                resource.nested_invocation_id,
                                resource.target_function_id,
                                resource.source_revision_id,
                                resource.catalogue_revision_id,
                                resource.session_principal_id,
                                resource.decision_outcome,
                                resource.terminal_outcome,
                                invocation.outcome AS invocation_outcome,
                                invocation.function_id AS invocation_function_id,
                                invocation.source_revision_id AS invocation_source_revision_id,
                                invocation.catalogue_revision_id AS invocation_catalogue_revision_id,
                                invocation.session_principal_id AS invocation_session_principal_id,
                                invocation.effective_principal_id AS invocation_effective_principal_id
                         FROM _orna_kernel.resource_audit_events AS resource
                         LEFT JOIN _orna_kernel.invocation_audit_events AS invocation
                           ON invocation.invocation_id = resource.nested_invocation_id
                         WHERE resource.request_id = $1",
                        &[&request_id_bytes],
                    )
                    .await?;
                let parent: Vec<u8> = row.try_get("parent_invocation_id")?;
                let recorded_call_site: Vec<u8> = row.try_get("call_site_id")?;
                let nested: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
                let recorded_target: Option<Vec<u8>> = row.try_get("target_function_id")?;
                let source_revision: Option<Vec<u8>> = row.try_get("source_revision_id")?;
                let catalogue_revision: Option<Vec<u8>> =
                    row.try_get("catalogue_revision_id")?;
                let principal: Vec<u8> = row.try_get("session_principal_id")?;
                let decision: String = row.try_get("decision_outcome")?;
                let terminal: String = row.try_get("terminal_outcome")?;
                let invocation_outcome: Option<String> = row.try_get("invocation_outcome")?;
                let invocation_function: Option<Vec<u8>> =
                    row.try_get("invocation_function_id")?;
                let invocation_source_revision: Option<Vec<u8>> =
                    row.try_get("invocation_source_revision_id")?;
                let invocation_catalogue_revision: Option<Vec<u8>> =
                    row.try_get("invocation_catalogue_revision_id")?;
                let invocation_session_principal: Option<Vec<u8>> =
                    row.try_get("invocation_session_principal_id")?;
                let invocation_effective_principal: Option<Vec<u8>> =
                    row.try_get("invocation_effective_principal_id")?;
                require(
                    parent == parent_invocation.to_bytes().to_vec()
                        && recorded_call_site == call_site.to_bytes().to_vec()
                        && nested.as_ref().is_some_and(|nested| {
                            nested.len() == 16
                                && nested.as_slice() != parent_invocation.to_bytes()
                                && nested.as_slice() != request_id.to_bytes()
                        }),
                    "stream resource audit lost parent, call-site, or nested invocation identity",
                )?;
                require(
                    recorded_target == Some(target.to_bytes().to_vec())
                        && source_revision == Some(active.pair().source().to_bytes().to_vec())
                        && catalogue_revision
                            == Some(active.pair().catalogue().to_bytes().to_vec())
                        && principal == RAW_CLIENT_USER.to_bytes().to_vec()
                        && decision == "allowed"
                        && terminal == "completed"
                        && invocation_outcome.as_deref() == Some("allowed")
                        && invocation_function == Some(target.to_bytes().to_vec())
                        && invocation_source_revision
                            == Some(active.pair().source().to_bytes().to_vec())
                        && invocation_catalogue_revision
                            == Some(active.pair().catalogue().to_bytes().to_vec())
                        && invocation_session_principal == Some(RAW_CLIENT_USER.to_bytes().to_vec())
                        && invocation_effective_principal
                            == Some(RAW_CLIENT_USER.to_bytes().to_vec()),
                    "stream resource audit lost target, revision, principal, or terminal evidence",
                )
            }
            .await;
            finish_session(
                audit_operation,
                audit_session.shutdown().await,
                "stream resource audit lookup",
            )?;
            let terminal_result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                &[],
                &[],
                &grants,
                &mut state,
                parent_invocation,
                &mut executor,
            )?;
            let expected_terminal = RuntimeValue::option(
                &active,
                TypeDescriptor::option(TypeDescriptor::list(TypeDescriptor::named(
                    orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                ))?)?,
                None,
            )?;
            require(
                terminal_result.value() == &expected_terminal,
                "next installed stream AWAIT did not return typed terminal None",
            )?;
            require(
                state.resource(resource_key) == Some(&terminal_state),
                "terminal None AWAIT mutated the completed stream resource",
            )?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }
        .await;
        drop(executor);
        finish_session(operation, Ok(()), "installed stream evaluator cleanup")?;
        require_no_database_sessions(&database).await
    })
    .await
}
