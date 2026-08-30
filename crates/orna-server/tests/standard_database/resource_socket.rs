#![cfg(feature = "test-hooks")]

use super::*;

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn installed_resource_socket_delivers_values_and_enforces_windows_and_grants() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("installed-resource-live".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!("build installed resource runtime failed: {error}"))
                })?;
            runtime.block_on(
                installed_resource_socket_delivers_values_and_enforces_windows_and_grants_inner(),
            )
        })
        .map_err(|error| failure(format!("spawn installed resource thread failed: {error}")))?;
    handle
        .join()
        .map_err(|_| failure("installed resource thread panicked"))?
}

#[cfg(feature = "test-hooks")]
async fn installed_resource_socket_delivers_values_and_enforces_windows_and_grants_inner()
-> TestResult<()> {
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
            .ok_or_else(|| failure("installed resource fixture has no checked standard source"))?;
        let checked_standard = check_standard_library_source(&standard_source)
            .map_err(|error| failure(format!("installed standard source check failed: {error:?}")))?;
        let (active, _client_function, target, parameter, call_site) =
            install_stream_resource_client_fixture(&kernel, &active, &checked_standard)
                .await
                .map_err(|error| failure(format!("install installed stream fixture failed: {error:?}")))?;
        let all_target = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "all"])
            .ok_or_else(|| failure("installed resource fixture is missing resource_fixture.all"))?
            .id();
        let root = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "root"])
            .ok_or_else(|| failure("installed resource fixture is missing resource_fixture.root"))?
            .id();
        let probe_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["resource_fixture", "probe"])
            .ok_or_else(|| failure("installed resource fixture is missing resource_fixture.probe"))?
            .id();
        let probe_relation = format!(
            "_orna_data.t_{:032x}",
            u128::from_be_bytes(probe_type.to_bytes()),
        );
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "create"])
            .ok_or_else(|| failure("installed resource fixture is missing resource_fixture.create"))?
            .id();
        let create_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource_fixture.create is absent from the active catalogue"))?
            .parameter_by_name("p_marker")
            .ok_or_else(|| failure("resource_fixture.create.p_marker is absent from the active catalogue"))?
            .id();
        let sequence_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource_fixture.create is absent from the active catalogue"))?
            .parameter_by_name("p_sequence")
            .ok_or_else(|| failure("resource_fixture.create.p_sequence is absent from the active catalogue"))?
            .id();
        for (sequence, marker) in ["resource-value", "resource-value-2"].into_iter().enumerate() {
            kernel
                .execute_server_insert(
                    create,
                    &[
                        FunctionArgument::new(
                            create_parameter,
                            RuntimeValue::Text(marker.into()),
                        )?,
                        FunctionArgument::new(
                            sequence_parameter,
                            RuntimeValue::Integer((sequence + 1) as i32),
                        )?,
                    ],
                )
                .await
                .map_err(|error| failure(format!("insert resource fixture row failed: {error:?}")))?;
        }

        let mut functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
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
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let registry = registered_opaque_codecs(&standard_source)?;
        let granted_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, target),
                ExecuteGrant::new(RAW_CLIENT_USER, all_target),
                ExecuteGrant::new(RAW_CLIENT_USER, root),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&granted_security).await?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let root_request = sealed_scalar_resource_request(root)?;
        let retained_root = encode_invoke_request(&active, &registry, &root_request)?;
        let root_result = kernel
            .dispatch_sealed_sys_invoke(&session, 5, &retained_root)
            .await?;
        let parent_invocation_id = match root_result {
            SealedInvocationResult::Completed { invocation, .. } => invocation,
            result => {
                return Err(failure(format!(
                    "installed resource fixture root did not complete through sealed invoke: {result:?}"
                )));
            }
        };


        let first_value_bytes = exact_resource_value_bytes(
            &active,
            &registry,
            &ResourceServerFrame::Values(orna_protocol::ResourceValues {
                stream_id: 2,
                request_id: InvocationId::from_bytes([0x51; 16]),
                target_revision: active.pair(),
                batch_sequence: 0,
                item_count: 1,
                byte_count: 0,
                values: vec![RuntimeValue::Text("resource-value".into())],
            }),
        )?;
        let stream_request = ResourceRequest {
            stream_id: 2,
            request_id: InvocationId::from_bytes([0x51; 16]),
            parent_invocation_id,
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: all_target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![],
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        };
        let item_barrier_request = ResourceRequest {
            stream_id: 3,
            request_id: InvocationId::from_bytes([0x53; 16]),
            parent_invocation_id,
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text("resource-value".into()),
            }],
            item_window: 1,
            byte_window: 1,
        };
        let byte_request = ResourceRequest {
            stream_id: 4,
            request_id: InvocationId::from_bytes([0x55; 16]),
            parent_invocation_id,
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: all_target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![],
            item_window: MAX_RESOURCE_WINDOW,
            byte_window: first_value_bytes as u64,
        };
        let byte_barrier_request = ResourceRequest {
            stream_id: 5,
            request_id: InvocationId::from_bytes([0x57; 16]),
            parent_invocation_id,
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text("resource-value".into()),
            }],
            item_window: 1,
            byte_window: 1,
        };
        let authorizer = RawResourceRequestAuthorizer::new();
        for request in [
            &stream_request,
            &item_barrier_request,
            &byte_request,
            &byte_barrier_request,
        ] {
            require(
                authorizer.expect(request),
                "installed resource socket test could not register its request",
            )?;
        }
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer,
        ));
        let stream_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "resource stream socket did not complete the constructed handshake",
            )?;

            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(stream_request.clone()),
            )
            .await?;
            let accepted = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            if !matches!(
                &accepted,
                ResourceServerFrame::Accepted(frame) if frame.stream_id == 2
            ) {
                return Err(failure(format!(
                    "resource stream socket returned an unexpected acceptance frame: {accepted:?}",
                )));
            }
            let (_, first_values) =
                read_resource_server_frame_with_encoded(&mut client, &active, &registry).await?;
            require(
                matches!(
                    &first_values,
                    ResourceServerFrame::Values(frame)
                        if frame.stream_id == 2
                            && frame.batch_sequence == 0
                            && frame.item_count == 1
                            && frame.byte_count as usize == first_value_bytes
                            && frame.values == [RuntimeValue::Text("resource-value".into())]
                ),
                "resource stream socket did not return the exact first item-credit batch",
            )?;

            // The barrier has one byte of credit, so it cannot publish its
            // value until the test restores that credit.
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: 2,
                    request_id: stream_request.request_id,
                    add_items: 0,
                    add_bytes: first_value_bytes as u64,
                }),
            )
            .await?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(item_barrier_request.clone()),
            )
            .await?;
            let item_barrier_accepted =
                timeout(
                    Duration::from_secs(5),
                    read_resource_server_frame_from_socket(&mut client, &active, &registry),
                )
                .await
                .map_err(|_| failure("item barrier acceptance timed out"))??;
            require(
                matches!(
                    item_barrier_accepted,
                    ResourceServerFrame::Accepted(frame) if frame.stream_id == item_barrier_request.stream_id
                ),
                "item-only restoration released a stream with exhausted item credit",
            )?;

            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: 2,
                    request_id: stream_request.request_id,
                    add_items: 1,
                    add_bytes: 0,
                }),
            )
            .await?;
            let (_, second_values) =
                read_resource_server_frame_with_encoded(&mut client, &active, &registry).await?;
            let expected_second_bytes = exact_resource_value_bytes(&active, &registry, &second_values)?;
            require(
                matches!(
                    &second_values,
                    ResourceServerFrame::Values(frame)
                        if frame.stream_id == 2
                            && frame.batch_sequence == 1
                            && frame.item_count == 1
                            && frame.byte_count as usize == expected_second_bytes
                            && frame.values == [RuntimeValue::Text("resource-value-2".into())]
                ),
                "item-credit restoration did not resume the exact second batch",
            )?;
            let completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(
                    completed,
                    ResourceServerFrame::Completed(frame)
                        if frame.stream_id == 2
                            && frame.final_batch_sequence == 1
                            && frame.total_items == 2
                ),
                "resource stream socket did not complete after item-credit restoration",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: item_barrier_request.stream_id,
                    request_id: item_barrier_request.request_id,
                    add_items: 0,
                    add_bytes: MAX_RESOURCE_WINDOW - 1,
                }),
            )
            .await?;
            let barrier_values = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(barrier_values, ResourceServerFrame::Values(frame) if frame.stream_id == 3),
                "item-credit barrier stream did not receive its typed SERVER result",
            )?;
            let barrier_completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(barrier_completed, ResourceServerFrame::Completed(frame) if frame.stream_id == 3),
                "item-credit barrier stream did not complete",
            )?;

            // Start a second stream with exactly one value's byte credit but
            // ample item credit. Restoring only item credit must not release it.
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(byte_request.clone()),
            )
            .await?;
            let accepted = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(accepted, ResourceServerFrame::Accepted(frame) if frame.stream_id == 4),
                "resource stream socket did not accept the byte-credit request",
            )?;
            let (_, first_byte_values) =
                read_resource_server_frame_with_encoded(&mut client, &active, &registry).await?;
            require(
                matches!(
                    &first_byte_values,
                    ResourceServerFrame::Values(frame)
                        if frame.stream_id == 4
                            && frame.batch_sequence == 0
                            && frame.item_count == 1
                            && frame.byte_count as usize == first_value_bytes
                            && frame.values == [RuntimeValue::Text("resource-value".into())]
                ),
                "byte-credit request did not consume exactly its initial byte credit",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: 4,
                    request_id: byte_request.request_id,
                    add_items: 1,
                    add_bytes: 0,
                }),
            )
            .await?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(byte_barrier_request.clone()),
            )
            .await?;
            let byte_barrier_accepted = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(
                    byte_barrier_accepted,
                    ResourceServerFrame::Accepted(frame) if frame.stream_id == byte_barrier_request.stream_id
                ),
                "byte-credit restoration released a stream with exhausted byte credit",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: 4,
                    request_id: byte_request.request_id,
                    add_items: 0,
                    add_bytes: MAX_RESOURCE_WINDOW,
                }),
            )
            .await?;
            let (_, second_byte_values) =
                read_resource_server_frame_with_encoded(&mut client, &active, &registry).await?;
            let expected_second_byte_values =
                exact_resource_value_bytes(&active, &registry, &second_byte_values)?;
            require(
                matches!(
                    &second_byte_values,
                    ResourceServerFrame::Values(frame)
                        if frame.stream_id == 4
                            && frame.batch_sequence == 1
                            && frame.item_count == 1
                            && frame.byte_count as usize == expected_second_byte_values
                            && frame.values == [RuntimeValue::Text("resource-value-2".into())]
                ),
                "byte-credit restoration did not resume the exact second batch",
            )?;
            let completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(completed, ResourceServerFrame::Completed(frame) if frame.stream_id == 4 && frame.total_items == 2),
                "byte-credit stream did not complete after restoration",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: byte_barrier_request.stream_id,
                    request_id: byte_barrier_request.request_id,
                    add_items: 0,
                    add_bytes: MAX_RESOURCE_WINDOW - 1,
                }),
            )
            .await?;
            let barrier_values = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(barrier_values, ResourceServerFrame::Values(frame) if frame.stream_id == 5),
                "byte-credit barrier stream did not receive its typed SERVER result",
            )?;
            let barrier_completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(barrier_completed, ResourceServerFrame::Completed(frame) if frame.stream_id == 5),
                "byte-credit barrier stream did not complete",
            )?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            stream_operation,
            finish_session(shutdown, connection, "stream resource socket cleanup"),
            "stream resource socket operation",
        )?;

        let waiter_session = database.open().await?;
        let cancellation_request = ResourceRequest {
            stream_id: 6,
            request_id: InvocationId::from_bytes([0x61; 16]),
            parent_invocation_id,
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: all_target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![],
            item_window: 1,
            byte_window: 1,
        };
        let authorizer = RawResourceRequestAuthorizer::new();
        require(
            authorizer.expect(&cancellation_request),
            "cancellation resource socket test could not register its request",
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer.clone(),
        ));
        let cancellation_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "cancellation socket did not complete the constructed handshake",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(cancellation_request.clone()),
            )
            .await?;
            let accepted = timeout(
                Duration::from_secs(5),
                read_resource_server_frame_from_socket(&mut client, &active, &registry),
            )
            .await
            .map_err(|_| failure("cancellation resource did not reach acceptance"))??;
            require(
                matches!(
                    accepted,
                    ResourceServerFrame::Accepted(frame)
                        if frame.stream_id == cancellation_request.stream_id
                            && frame.request_id == cancellation_request.request_id
                            && frame.target_revision == cancellation_request.target_revision
                ),
                "cancellation resource did not observe RESOURCE_ACCEPTED",
            )?;
            let resource_query = format!("%{probe_relation}%");
            let producer_active = timeout(Duration::from_secs(5), async {
                loop {
                    let active = waiter_session
                        .client()
                        .query_one(
                            "SELECT EXISTS (
                                SELECT 1
                                FROM pg_stat_activity
                                WHERE pid <> pg_backend_pid()
                                  AND datname = current_database()
                                  AND state = 'idle in transaction'
                                  AND query LIKE $1
                            )",
                            &[&resource_query],
                        )
                        .await?
                        .get::<_, bool>(0);
                    if active {
                        return Ok::<(), tokio_postgres::Error>(());
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("cancellation resource did not reach its active query"))?;
            producer_active?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Cancel(ResourceCancel {
                    stream_id: cancellation_request.stream_id,
                    request_id: cancellation_request.request_id,
                    reason: ResourceCancellationCode::ClientRequested,
                }),
            )
            .await?;
            let cancelled = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(
                    cancelled,
                    ResourceServerFrame::Cancelled(frame)
                        if frame.stream_id == cancellation_request.stream_id
                            && frame.request_id == cancellation_request.request_id
                            && frame.target_revision == cancellation_request.target_revision
                            && frame.reason == ResourceCancellationCode::ClientRequested
                ),
                "active authenticated resource dispatch did not terminate as cancelled",
            )?;

            let replacement_request = ResourceRequest {
                stream_id: 8,
                request_id: InvocationId::from_bytes([0x81; 16]),
                parent_invocation_id,
                call_site_id: call_site,
                state_profile: String::new(),
                function_instance_key: String::new(),
                target_function_id: target,
                target_revision: active.pair(),
                generation: 1,
                resource_kind: ResourceKind::Stream,
                arguments: vec![ResourceArgument { parameter, value: RuntimeValue::Text("resource-value".into()) }],
                item_window: 1,
                byte_window: MAX_RESOURCE_WINDOW,
            };
            require(
                authorizer.expect(&replacement_request),
                "replacement resource socket test could not register its request",
            )?;
            send_resource_client_frame_to_socket(&mut client, &active, &registry, &ResourceClientFrame::Request(replacement_request.clone())).await?;
            let replacement_accepted = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(matches!(replacement_accepted, ResourceServerFrame::Accepted(frame) if frame.stream_id == replacement_request.stream_id), "resource executor was not reusable after cancellation")?;
            let replacement_values = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(matches!(replacement_values, ResourceServerFrame::Values(frame) if frame.stream_id == replacement_request.stream_id && frame.values == [RuntimeValue::Text("resource-value".into())]), "replacement request did not return its typed value")?;
            let replacement_completed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(matches!(replacement_completed, ResourceServerFrame::Completed(frame) if frame.stream_id == replacement_request.stream_id && frame.total_items == 1), "replacement request did not complete after cancellation")
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let waiter_shutdown = waiter_session.shutdown();
        finish_session(
            cancellation_operation,
            finish_session(
                shutdown,
                connection,
                "cancellation resource socket cleanup",
            ),
            "cancellation resource socket operation",
        )?;
        waiter_shutdown.await?;

        let denied_security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&denied_security).await?;
        let denied_request = ResourceRequest {
            stream_id: 7,
            request_id: InvocationId::from_bytes([0x71; 16]),
            parent_invocation_id,
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text("resource-value".into()),
            }],
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        };
        let authorizer = RawResourceRequestAuthorizer::new();
        require(
            authorizer.expect(&denied_request),
            "denied resource socket test could not register its request",
        )?;
        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer,
        ));
        let denied_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "denied resource socket did not complete the constructed handshake",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(denied_request.clone()),
            )
            .await?;
            let failed = read_resource_server_frame_from_socket(&mut client, &active, &registry).await?;
            require(
                matches!(
                    failed,
                    ResourceServerFrame::Failed(frame)
                        if frame.stream_id == denied_request.stream_id
                            && frame.request_id == denied_request.request_id
                            && frame.failure == CallFailure::ExecuteDenied
                ),
                "resource socket did not return execute denial",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            denied_operation,
            finish_session(shutdown, connection, "denied resource socket cleanup"),
            "denied resource socket operation",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let stream_allows = audits.iter().filter(|audit| {
            let decision = audit.decision();
            decision.kind() == SecurityAuditKind::Execute
                && decision.outcome() == SecurityAuditOutcome::Allowed
                && decision.target() == Some(InvocationTarget::new(all_target, active.pair()))
        }).count();
        let denied = audits.iter().find(|audit| {
            let decision = audit.decision();
            decision.kind() == SecurityAuditKind::Execute
                && decision.outcome() == SecurityAuditOutcome::Denied
                && decision.target() == Some(InvocationTarget::new(target, active.pair()))
        });
        // The cancelled request loses its uncommitted allowed decision when
        // cancellation aborts the blocked execution transaction.
        require(
            stream_allows >= 2
                && denied.is_some_and(|audit| {
                    audit.decision().denial()
                        == Some(SecurityAuditDenial::Execute(ExecuteDenial::MissingExecuteGrant))
                    && audit.decision().effective_principal().is_none()
                    && audit.decision().authorising_principal().is_none()
                }),
            "resource socket audit evidence did not record stream allows and redacted denial",
        )?;
        let audit_text = format!("{audits:?}");
        require(
            !audit_text.contains("resource-value")
                && !audit_text.contains("resource-value-2"),
            "resource audit evidence retained raw argument or result detail",
        )?;

        assert_resource_audit_rows(
            &database,
            &active,
            call_site,
            target,
            all_target,
            parent_invocation_id,
        )
        .await?;

        // The terminal rows must remain queryable after the current kernel and
        // audit session are gone and the installed standard is recovered by a
        // fresh kernel instance.
        drop(kernel);
        let recovered_kernel = open_standard_database(database.connection_string().parse()?)
            .await
            .map_err(|error| failure(format!("reopen standard database failed: {error:?}")))?;
        let recovered_active = recovered_kernel
            .recover()
            .await
            .map_err(|error| failure(format!("recover installed standard after reopen failed: {error:?}")))?;
        require(
            recovered_active.pair() == active.pair(),
            "fresh recovery returned a different active revision pair",
        )?;
        assert_resource_audit_rows(
            &database,
            &recovered_active,
            call_site,
            target,
            all_target,
            parent_invocation_id,
        )
        .await?;
        drop(recovered_kernel);

        Ok(())
    })
    .await
}

/// Proves cancellation and request replacement through the public raw
/// resource socket. The integration crate cannot call the crate-private
/// `InstalledClientResourceExecutor::new_with_broker`; the public test-hook
/// raw socket adapter therefore drives the same authenticated resource channel
/// directly with canonical protocol frames.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn installed_resource_socket_cancellation_reclaims_and_replaces_request() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("installed-resource-socket-cancellation-live".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "build installed resource socket cancellation runtime failed: {error}"
                    ))
                })?;
            runtime.block_on(
                installed_resource_socket_cancellation_reclaims_and_replaces_request_inner(),
            )
        })
        .map_err(|error| {
            failure(format!(
                "spawn installed resource socket cancellation thread failed: {error}"
            ))
        })?;
    handle
        .join()
        .map_err(|_| failure("installed resource socket cancellation thread panicked"))?
}

#[cfg(feature = "test-hooks")]
async fn installed_resource_socket_cancellation_reclaims_and_replaces_request_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
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
            .ok_or_else(|| failure("resource cancellation fixture has no checked standard source"))?;
        let checked_standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!("installed standard source check failed: {error:?}"))
        })?;
        let (active, _client_function, target, parameter, call_site) =
            install_stream_resource_client_fixture(&kernel, &active, &checked_standard).await?;
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "create"])
            .ok_or_else(|| failure("resource cancellation fixture is missing resource_fixture.create"))?
            .id();
        let create_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource cancellation fixture create function disappeared"))?
            .parameter_by_name("p_marker")
            .ok_or_else(|| failure("resource cancellation fixture create marker parameter disappeared"))?
            .id();
        let sequence_parameter = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource cancellation fixture create function disappeared"))?
            .parameter_by_name("p_sequence")
            .ok_or_else(|| failure("resource cancellation fixture create sequence parameter disappeared"))?
            .id();
        let root = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "root"])
            .ok_or_else(|| failure("resource cancellation fixture is missing resource_fixture.root"))?
            .id();
        kernel
            .execute_server_insert(
                create,
                &[
                    FunctionArgument::new(
                        create_parameter,
                        RuntimeValue::Text("cancel-replacement-value".to_owned()),
                    )?,
                    FunctionArgument::new(sequence_parameter, RuntimeValue::Integer(1))?,
                ],
            )
            .await
            .map_err(|error| failure(format!("insert cancellation fixture row failed: {error:?}")))?;
        let probe_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["resource_fixture", "probe"])
            .ok_or_else(|| failure("resource cancellation fixture is missing resource_fixture.probe"))?
            .id();
        let probe_relation = format!(
            "_orna_data.t_{:032x}",
            u128::from_be_bytes(probe_type.to_bytes())
        );

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
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, target),
                ExecuteGrant::new(RAW_CLIENT_USER, root),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let registry = registered_opaque_codecs(&standard_source)?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let root_request = sealed_scalar_resource_request(root)?;
        let retained_root = encode_invoke_request(&active, &registry, &root_request)?;
        let root_result = kernel
            .dispatch_sealed_sys_invoke(&session, 5, &retained_root)
            .await?;
        let parent_invocation_id = match root_result {
            SealedInvocationResult::Completed { invocation, .. } => invocation,
            result => {
                return Err(failure(format!(
                    "resource cancellation fixture root did not complete through sealed invoke: {result:?}"
                )));
            }
        };

        let first_request = ResourceRequest {
            stream_id: 1,
            request_id: InvocationId::new(),
            parent_invocation_id,
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text("cancel-replacement-value".to_owned()),
            }],
            item_window: 1,
            byte_window: 1,
        };
        let mut replacement_request = first_request.clone();
        replacement_request.stream_id = 2;
        replacement_request.request_id = InvocationId::new();
        replacement_request.generation = 2;
        replacement_request.byte_window = MAX_RESOURCE_WINDOW;
        let authorizer = RawResourceRequestAuthorizer::new();
        require(
            authorizer.expect(&first_request),
            "resource cancellation proof could not register its first request",
        )?;

        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer.clone(),
        ));
        let waiter_session = database.open().await?;
        let stream_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "resource cancellation proof did not complete the constructed handshake",
            )?;

            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(first_request.clone()),
            )
            .await?;
            let accepted = timeout(
                Duration::from_secs(5),
                read_resource_server_frame_from_socket(&mut client, &active, &registry),
            )
            .await
            .map_err(|_| failure("resource cancellation proof did not reach acceptance"))??;
            require(
                matches!(
                    accepted,
                    ResourceServerFrame::Accepted(frame)
                        if frame.stream_id == first_request.stream_id
                            && frame.request_id == first_request.request_id
                            && frame.target_revision == first_request.target_revision
                            && frame.resource_kind == first_request.resource_kind
                ),
                "resource cancellation proof did not observe RESOURCE_ACCEPTED",
            )?;
            let resource_query = format!("%{probe_relation}%");
            let producer_active = timeout(Duration::from_secs(5), async {
                loop {
                    let active = waiter_session
                        .client()
                        .query_one(
                            "SELECT EXISTS (
                                SELECT 1
                                FROM pg_stat_activity
                                WHERE pid <> pg_backend_pid()
                                  AND datname = current_database()
                                  AND state = 'idle in transaction'
                                  AND query LIKE $1
                            )",
                            &[&resource_query],
                        )
                        .await?
                        .get::<_, bool>(0);
                    if active {
                        return Ok::<(), tokio_postgres::Error>(());
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("resource cancellation producer did not reach its active query"))?;
            producer_active?;


            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Cancel(ResourceCancel {
                    stream_id: first_request.stream_id,
                    request_id: first_request.request_id,
                    reason: ResourceCancellationCode::ClientRequested,
                }),
            )
            .await?;
            let cancelled = timeout(
                Duration::from_secs(5),
                read_resource_server_frame_from_socket(&mut client, &active, &registry),
            )
            .await
            .map_err(|_| failure("cancelled resource did not reach its terminal frame"))??;
            require(
                matches!(
                    cancelled,
                    ResourceServerFrame::Cancelled(frame)
                        if frame.stream_id == first_request.stream_id
                            && frame.request_id == first_request.request_id
                            && frame.target_revision == first_request.target_revision
                            && frame.reason == ResourceCancellationCode::ClientRequested
                ),
                "resource cancellation proof did not return the redacted terminal cancellation",
            )?;

            require(
                authorizer.expect(&replacement_request),
                "resource cancellation proof could not register its replacement request",
            )?;
            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(replacement_request.clone()),
            )
            .await?;
            let replacement_accepted = timeout(
                Duration::from_secs(5),
                read_resource_server_frame_from_socket(&mut client, &active, &registry),
            )
            .await
            .map_err(|_| failure("replacement resource did not reach acceptance"))??;
            require(
                matches!(
                    replacement_accepted,
                    ResourceServerFrame::Accepted(frame)
                        if frame.stream_id == replacement_request.stream_id
                            && frame.request_id == replacement_request.request_id
                            && frame.target_revision == replacement_request.target_revision
                ),
                "a late first-generation frame leaked into replacement acceptance",
            )?;
            let replacement_values = timeout(
                Duration::from_secs(5),
                read_resource_server_frame_from_socket(&mut client, &active, &registry),
            )
            .await
            .map_err(|_| failure("replacement resource did not publish its value"))??;
            require(
                matches!(
                    replacement_values,
                    ResourceServerFrame::Values(frame)
                        if frame.stream_id == replacement_request.stream_id
                            && frame.request_id == replacement_request.request_id
                            && frame.target_revision == replacement_request.target_revision
                            && frame.batch_sequence == 0
                            && frame.values
                                == [RuntimeValue::Text("cancel-replacement-value".to_owned())]
                ),
                "replacement resource did not publish its typed value without stale leakage",
            )?;
            let replacement_completed = timeout(
                Duration::from_secs(5),
                read_resource_server_frame_from_socket(&mut client, &active, &registry),
            )
            .await
            .map_err(|_| failure("replacement resource did not complete"))??;
            require(
                matches!(
                    replacement_completed,
                    ResourceServerFrame::Completed(frame)
                        if frame.stream_id == replacement_request.stream_id
                            && frame.request_id == replacement_request.request_id
                            && frame.target_revision == replacement_request.target_revision
                            && frame.final_batch_sequence == 0
                            && frame.total_items == 1
                ),
                "replacement resource did not complete after cancellation cleanup",
            )
        }
        .await;
        let shutdown = client.shutdown().await.map_err(Into::into);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            stream_operation,
            finish_session(shutdown, connection, "resource cancellation socket cleanup"),
            "resource cancellation replacement operation",
        )?;
        waiter_session.shutdown().await?;

        let audit_session = database.open().await?;
        let audit_operation = async {
            let rows = audit_session
                .client()
                .query(
                    "SELECT resource.request_id, resource.parent_invocation_id,
                            resource.nested_invocation_id, resource.call_site_id,
                            resource.target_function_id, resource.source_revision_id,
                            resource.catalogue_revision_id, resource.session_principal_id,
                            resource.decision_outcome, resource.terminal_outcome,
                            resource.item_count, resource.byte_count,
                            invocation.invocation_id AS invocation_id,
                            invocation.outcome AS invocation_outcome,
                            invocation.session_principal_id AS invocation_principal_id,
                            invocation.function_id AS invocation_function_id,
                            invocation.source_revision_id AS invocation_source_revision_id,
                            invocation.catalogue_revision_id AS invocation_catalogue_revision_id
                     FROM _orna_kernel.resource_audit_events AS resource
                     LEFT JOIN _orna_kernel.invocation_audit_events AS invocation
                       ON invocation.invocation_id = resource.nested_invocation_id
                     ORDER BY resource.sequence",
                    &[],
                )
                .await?;
            require(
                rows.len() == 2,
                "resource cancellation proof did not retain exactly two terminal audit rows",
            )?;
            let target_bytes = target.to_bytes().to_vec();
            let source_revision_bytes = active.pair().source().to_bytes().to_vec();
            let catalogue_revision_bytes = active.pair().catalogue().to_bytes().to_vec();
            let principal_bytes = RAW_CLIENT_USER.to_bytes().to_vec();
            for (index, row) in rows.iter().enumerate() {
                let request_id: Vec<u8> = row.try_get("request_id")?;
                let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
                let nested_invocation_id: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
                let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
                let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
                let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
                let catalogue_revision_id: Option<Vec<u8>> =
                    row.try_get("catalogue_revision_id")?;
                let session_principal_id: Vec<u8> = row.try_get("session_principal_id")?;
                let decision_outcome: String = row.try_get("decision_outcome")?;
                let terminal_outcome: String = row.try_get("terminal_outcome")?;
                let item_count: Option<i64> = row.try_get("item_count")?;
                let byte_count: Option<i64> = row.try_get("byte_count")?;
                let invocation_outcome: Option<String> = row.try_get("invocation_outcome")?;
                let invocation_id: Option<Vec<u8>> = row.try_get("invocation_id")?;
                let invocation_principal_id: Option<Vec<u8>> =
                    row.try_get("invocation_principal_id")?;
                let invocation_function_id: Option<Vec<u8>> =
                    row.try_get("invocation_function_id")?;
                let invocation_source_revision_id: Option<Vec<u8>> =
                    row.try_get("invocation_source_revision_id")?;
                let invocation_catalogue_revision_id: Option<Vec<u8>> =
                    row.try_get("invocation_catalogue_revision_id")?;
                let (request, parent, decision, terminal, expected_items, target_present) =
                    if index == 0 {
                        (
                            &first_request,
                            &first_request.parent_invocation_id,
                            "allowed",
                            "cancelled",
                            None,
                            true,
                        )
                    } else {
                        (
                            &replacement_request,
                            &replacement_request.parent_invocation_id,
                            "allowed",
                            "completed",
                            Some(1_i64),
                            true,
                        )
                    };
                let target_matches = if target_present {
                    target_function_id.as_deref() == Some(target_bytes.as_slice())
                        && source_revision_id.as_deref() == Some(source_revision_bytes.as_slice())
                        && catalogue_revision_id.as_deref()
                            == Some(catalogue_revision_bytes.as_slice())
                } else {
                    target_function_id.is_none()
                        && source_revision_id.is_none()
                        && catalogue_revision_id.is_none()
                };
                let nested_identity_matches = target_present
                    && nested_invocation_id.as_ref().is_some_and(|id| id.len() == 16)
                    && nested_invocation_id == invocation_id
                    && invocation_outcome.as_deref() == Some(decision)
                    && invocation_principal_id.as_deref() == Some(principal_bytes.as_slice())
                    && invocation_function_id.as_deref() == Some(target_bytes.as_slice())
                    && invocation_source_revision_id.as_deref()
                        == Some(source_revision_bytes.as_slice())
                    && invocation_catalogue_revision_id.as_deref()
                        == Some(catalogue_revision_bytes.as_slice());
                require(
                    request_id == request.request_id.to_bytes()
                        && parent_invocation_id == parent.to_bytes()
                        && nested_identity_matches
                        && call_site_id == call_site.to_bytes()
                        && target_matches
                        && session_principal_id == RAW_CLIENT_USER.to_bytes()
                        && decision_outcome == decision
                        && terminal_outcome == terminal
                        && item_count == expected_items
                        && (expected_items.is_none() || byte_count.is_some_and(|bytes| bytes > 0))
                        && invocation_outcome.as_deref() == target_present.then_some(decision),
                    "resource cancellation audit retained stale, unredacted, or mismatched terminal state",
                )?;
            }
            let history_count: i64 = audit_session
                .client()
                .query_one(
                    "SELECT count(*) FROM _orna_kernel.resource_request_history",
                    &[],
                )
                .await?
                .try_get(0)?;
            require(
                history_count == 2,
                "resource cancellation cleanup left a stale request reservation",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "resource cancellation audit cleanup",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}

/// Proves that an authenticated accepted SERVER resource producer is cancelled
/// when its client half-closes while the producer is active. The closed socket
/// must not receive a late terminal frame, and the cleanup must retain one
/// allowed terminal audit row linked to the accepted nested invocation.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn installed_resource_socket_disconnect_cancels_active_producer_and_audits() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("installed-resource-socket-disconnect-live".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "build installed resource socket disconnect runtime failed: {error}"
                    ))
                })?;
            runtime.block_on(
                installed_resource_socket_disconnect_cancels_active_producer_and_audits_inner(),
            )
        })
        .map_err(|error| {
            failure(format!(
                "spawn installed resource socket disconnect thread failed: {error}"
            ))
        })?;
    handle
        .join()
        .map_err(|_| failure("installed resource socket disconnect thread panicked"))?
}

#[cfg(feature = "test-hooks")]
async fn installed_resource_socket_disconnect_cancels_active_producer_and_audits_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
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
            .ok_or_else(|| failure("resource disconnect fixture has no checked standard source"))?;
        let checked_standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!("installed standard source check failed: {error:?}"))
        })?;
        let (active, _client_function, target, parameter, call_site) =
            install_stream_resource_client_fixture(&kernel, &active, &checked_standard).await?;
        let root = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "root"])
            .ok_or_else(|| failure("resource disconnect fixture is missing resource_fixture.root"))?
            .id();
        let create = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["resource_fixture", "create"])
            .ok_or_else(|| failure("resource disconnect fixture is missing resource_fixture.create"))?
            .id();
        let create_definition = active
            .catalogue()
            .function_by_id(create)
            .ok_or_else(|| failure("resource disconnect fixture create function disappeared"))?;
        let create_parameter = create_definition
            .parameter_by_name("p_marker")
            .ok_or_else(|| failure("resource disconnect fixture marker parameter disappeared"))?
            .id();
        let sequence_parameter = create_definition
            .parameter_by_name("p_sequence")
            .ok_or_else(|| failure("resource disconnect fixture sequence parameter disappeared"))?
            .id();
        kernel
            .execute_server_insert(
                create,
                &[
                    FunctionArgument::new(
                        create_parameter,
                        RuntimeValue::Text("disconnect-active-value".to_owned()),
                    )?,
                    FunctionArgument::new(sequence_parameter, RuntimeValue::Integer(1))?,
                ],
            )
            .await
            .map_err(|error| failure(format!("insert resource disconnect fixture row failed: {error:?}")))?;
        let probe_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["resource_fixture", "probe"])
            .ok_or_else(|| failure("resource disconnect fixture is missing resource_fixture.probe"))?
            .id();
        let probe_relation = format!(
            "_orna_data.t_{:032x}",
            u128::from_be_bytes(probe_type.to_bytes())
        );
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
        let uid = nix::unistd::getuid().as_raw();
        let principal = Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        );
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![principal],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, target),
                ExecuteGrant::new(RAW_CLIENT_USER, root),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let registry = registered_opaque_codecs(&standard_source)?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let root_request = sealed_scalar_resource_request(root)?;
        let retained_root = encode_invoke_request(&active, &registry, &root_request)?;
        let root_result = kernel
            .dispatch_sealed_sys_invoke(&session, 5, &retained_root)
            .await?;
        let parent_invocation_id = match root_result {
            SealedInvocationResult::Completed { invocation, .. } => invocation,
            result => {
                return Err(failure(format!(
                    "resource disconnect fixture root did not complete through sealed invoke: {result:?}"
                )));
            }
        };
        let request = ResourceRequest {
            stream_id: 1,
            request_id: InvocationId::new(),
            parent_invocation_id,
            call_site_id: call_site,
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target,
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Stream,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text("disconnect-active-value".to_owned()),
            }],
            item_window: 1,
            byte_window: 1,
        };
        let authorizer = RawResourceRequestAuthorizer::new();
        require(
            authorizer.expect(&request),
            "resource disconnect proof could not register its request",
        )?;

        let (server, client) = StandardUnixStream::pair()?;
        client.set_nonblocking(true)?;
        let mut client = UnixStream::from_std(client)?;
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer,
        ));
        let mut accepted_nested_bytes: Option<Vec<u8>> = None;
        let mut write_closed = false;
        let waiter_session = database.open().await?;
        let stream_operation = async {
            client
                .write_all(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00")
                .await?;
            let mut acknowledgement = [0_u8; 12];
            client.read_exact(&mut acknowledgement).await?;
            require(
                acknowledgement == *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00",
                "resource disconnect proof did not complete the constructed handshake",
            )?;

            send_resource_client_frame_to_socket(
                &mut client,
                &active,
                &registry,
                &ResourceClientFrame::Request(request.clone()),
            )
            .await?;
            let accepted = timeout(
                Duration::from_secs(5),
                read_resource_server_frame_from_socket(&mut client, &active, &registry),
            )
            .await
            .map_err(|_| failure("resource disconnect producer did not reach acceptance"))??;
            let accepted_nested_invocation_id = match accepted {
                ResourceServerFrame::Accepted(frame)
                    if frame.stream_id == request.stream_id
                        && frame.request_id == request.request_id
                        && frame.target_revision == request.target_revision
                        && frame.resource_kind == request.resource_kind => frame.nested_invocation_id,
                _ => return Err(failure("resource disconnect proof did not observe RESOURCE_ACCEPTED")),
            };
            let resource_query = format!("%{probe_relation}%");
            let producer_active = timeout(Duration::from_secs(5), async {
                loop {
                    let active = waiter_session
                        .client()
                        .query_one(
                            "SELECT EXISTS (
                                SELECT 1
                                FROM pg_stat_activity
                                WHERE pid <> pg_backend_pid()
                                  AND datname = current_database()
                                  AND state = 'idle in transaction'
                                  AND query LIKE $1
                            )",
                            &[&resource_query],
                        )
                        .await?
                        .get::<_, bool>(0);
                    if active {
                        return Ok::<(), tokio_postgres::Error>(());
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| failure("resource disconnect producer did not reach its active query"))?;
            producer_active?;
            accepted_nested_bytes = Some(accepted_nested_invocation_id.to_bytes().to_vec());


            client.shutdown().await?;
            write_closed = true;
            let mut trailing = Vec::new();
            timeout(Duration::from_secs(5), client.read_to_end(&mut trailing))
                .await
                .map_err(|_| failure("resource disconnect socket did not reach EOF"))??;
            require(
                trailing.is_empty(),
                "resource disconnect socket emitted a late terminal frame",
            )
        }
        .await;

        let shutdown = if write_closed {
            Ok(())
        } else {
            client.shutdown().await.map_err(Into::into)
        };
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        finish_session(
            stream_operation,
            finish_session(shutdown, connection, "resource disconnect socket cleanup"),
            "resource disconnect active producer operation",
        )?;
        waiter_session.shutdown().await?;
        let audit_session = database.open().await?;
        let audit_operation = async {
            let rows = audit_session
                .client()
                .query(
                    "SELECT resource.request_id, resource.parent_invocation_id,
                            resource.nested_invocation_id, resource.call_site_id,
                            resource.target_function_id, resource.source_revision_id,
                            resource.catalogue_revision_id, resource.session_principal_id,
                            resource.decision_outcome, resource.terminal_outcome,
                            resource.item_count, resource.byte_count,
                            invocation.invocation_id AS invocation_id,
                            invocation.outcome AS invocation_outcome,
                            invocation.session_principal_id AS invocation_principal_id,
                            invocation.function_id AS invocation_function_id,
                            invocation.source_revision_id AS invocation_source_revision_id,
                            invocation.catalogue_revision_id AS invocation_catalogue_revision_id
                     FROM _orna_kernel.resource_audit_events AS resource
                     LEFT JOIN _orna_kernel.invocation_audit_events AS invocation
                       ON invocation.invocation_id = resource.nested_invocation_id
                     ORDER BY resource.sequence",
                    &[],
                )
                .await?;
            require(
                rows.len() == 1,
                "resource disconnect proof did not retain exactly one terminal audit row",
            )?;
            let row = &rows[0];
            let request_id: Vec<u8> = row.try_get("request_id")?;
            let parent_invocation_id: Vec<u8> = row.try_get("parent_invocation_id")?;
            let nested_invocation_id: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
            let call_site_id: Vec<u8> = row.try_get("call_site_id")?;
            let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision_id: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
            let session_principal_id: Vec<u8> = row.try_get("session_principal_id")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            let invocation_id: Option<Vec<u8>> = row.try_get("invocation_id")?;
            let invocation_outcome: Option<String> = row.try_get("invocation_outcome")?;
            let invocation_principal_id: Option<Vec<u8>> =
                row.try_get("invocation_principal_id")?;
            let invocation_function_id: Option<Vec<u8>> = row.try_get("invocation_function_id")?;
            let invocation_source_revision_id: Option<Vec<u8>> =
                row.try_get("invocation_source_revision_id")?;
            let invocation_catalogue_revision_id: Option<Vec<u8>> =
                row.try_get("invocation_catalogue_revision_id")?;
            let target_bytes = target.to_bytes().to_vec();
            let source_revision_bytes = active.pair().source().to_bytes().to_vec();
            let catalogue_revision_bytes = active.pair().catalogue().to_bytes().to_vec();
            let principal_bytes = RAW_CLIENT_USER.to_bytes().to_vec();
            require(
                request_id == request.request_id.to_bytes()
                    && parent_invocation_id == request.parent_invocation_id.to_bytes()
                    && nested_invocation_id.as_deref() == accepted_nested_bytes.as_deref()
                    && nested_invocation_id.as_ref().is_some_and(|id| id.len() == 16)
                    && nested_invocation_id == invocation_id
                    && call_site_id == call_site.to_bytes()
                    && target_function_id.as_deref() == Some(target_bytes.as_slice())
                    && source_revision_id.as_deref() == Some(source_revision_bytes.as_slice())
                    && catalogue_revision_id.as_deref() == Some(catalogue_revision_bytes.as_slice())
                    && session_principal_id.as_slice() == principal_bytes.as_slice()
                    && decision_outcome == "allowed"
                    && terminal_outcome == "cancelled"
                    && item_count.is_none()
                    && byte_count.is_none()
                    && invocation_outcome.as_deref() == Some("allowed")
                    && invocation_principal_id.as_deref() == Some(principal_bytes.as_slice())
                    && invocation_function_id.as_deref() == Some(target_bytes.as_slice())
                    && invocation_source_revision_id.as_deref()
                        == Some(source_revision_bytes.as_slice())
                    && invocation_catalogue_revision_id.as_deref()
                        == Some(catalogue_revision_bytes.as_slice()),
                "resource disconnect audit lost accepted nested identity or terminal state",
            )?;
            let history_count: i64 = audit_session
                .client()
                .query_one(
                    "SELECT count(*) FROM _orna_kernel.resource_request_history",
                    &[],
                )
                .await?
                .try_get(0)?;
            require(
                history_count == 1,
                "resource disconnect cleanup did not retain exactly one request history row",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "resource disconnect audit cleanup",
        )?;
        require_no_database_sessions(&database).await
    })
    .await
}
