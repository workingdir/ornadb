use super::*;

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_server_action_resource_trigger_through_authenticated_executor() -> TestResult<()> {
    with_test_database(|database| async move {
        // This proof starts at the authenticated resource-trigger contract.
        // The sealed sys.invoke path evaluates a CLIENT root and returns its
        // opaque std.Action value, but it does not expose the
        // ClientExecutionContext or trigger that action. The direct sealed
        // SERVER dogfood proof above covers the outer sealed gate; keeping
        // those seams separate avoids inventing an action-trigger API here.
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_five = install_v5_standard(&kernel, &empty, &database).await?;
        let upgrade_v6 =
            orna_standard::prepare_standard_upgrade_v5_to_v6(&version_five).map_err(|error| {
                failure(format!(
                    "prepare V5-to-V6 standard upgrade failed: {error:?}"
                ))
            })?;
        let active = kernel
            .apply_standard_upgrade(&upgrade_v6)
            .await
            .map_err(|error| {
                failure(format!("apply V5-to-V6 standard upgrade failed: {error:?}"))
            })?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("action fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!("installed standard source check failed: {error:?}"))
        })?;
        let (
            active,
            client,
            target,
            client_parameter,
            target_parameter,
            local_client,
            local_target,
            local_client_parameter,
            local_target_parameter,
        ) = install_action_client_fixture(&kernel, &active, &standard).await?;
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.extend(standard.verified_snapshot().executables().iter().map(
            |executable| {
                SecurityFunctionTarget::verified_standard(
                    executable.function(),
                    standard.verified_snapshot().revision(),
                    executable.revision().id(),
                )
            },
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
                ExecuteGrant::new(RAW_CLIENT_USER, local_client),
                ExecuteGrant::new(RAW_CLIENT_USER, local_target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let (root_outcome, _, _) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(local_target),
                vec![CliArgumentInput::Canonical {
                    parameter: local_target_parameter.canonical(),
                    value: "43".to_owned(),
                }],
                true,
                false,
            ),
        )
        .await?;
        require(
            root_outcome == Ok(InstalledInvokeOutcome::Completed),
            "action evaluator could not create an owned root invocation",
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
                    &[
                        &local_target.to_bytes().to_vec(),
                        &RAW_CLIENT_USER.to_bytes().to_vec(),
                    ],
                )
                .await?;
            let bytes: Vec<u8> = row.try_get(0)?;
            let bytes: [u8; 16] = bytes
                .try_into()
                .map_err(|_| failure("action root invocation audit identity was not 16 bytes"))?;
            Ok(InvocationId::from_bytes(bytes))
        }
        .await;
        let parent_invocation = finish_session(
            root_operation,
            audit_session.shutdown().await,
            "action root invocation audit lookup",
        )?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let authorisation = match security
            .authorise_execute(&session, InvocationTarget::new(client, active.pair()))
        {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "installed action grant was denied: {denial:?}"
                )));
            }
        };
        let argument = FunctionArgument::new(client_parameter, RuntimeValue::Integer(43))?;
        let evaluation_grants = LocalCapabilityGrantSet::new();
        let mut evaluation_state = ClientStateStore::default();
        let mut evaluation_executor = DeterministicStreamResourceExecutor;
        let result =
            evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                std::slice::from_ref(&argument),
                &[],
                &evaluation_grants,
                &mut evaluation_state,
                parent_invocation,
                &mut evaluation_executor,
            )?;
        require(
            result.context().parent_invocation_id() == parent_invocation,
            "action evaluator did not retain its authenticated parent invocation",
        )?;
        let local_authorisation = match security
            .authorise_execute(&session, InvocationTarget::new(local_client, active.pair()))
        {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "installed local action grant was denied: {denial:?}"
                )));
            }
        };
        let local_argument =
            FunctionArgument::new(local_client_parameter, RuntimeValue::Integer(43))?;
        let local_result = evaluate_client_function_with_arguments(
            &active,
            &local_authorisation,
            std::slice::from_ref(&local_argument),
        )?;
        let RuntimeValue::Opaque(action) = result.value() else {
            return Err(failure(
                "action CLIENT function did not return an opaque action value",
            ));
        };
        let descriptor = decode_action_payload(&active, action.canonical_payload())?;
        require(
            descriptor.domain() == ActionTargetDomain::Server
                && descriptor.target() == target
                && descriptor.target_revision() == active.pair()
                && descriptor.arguments().len() == 1
                && descriptor.arguments()[0].parameter() == target_parameter
                && descriptor.arguments()[0].value() == argument.value(),
            "action value lost its authenticated SERVER target or canonical argument",
        )?;
        let RuntimeValue::Opaque(local_action) = local_result.value() else {
            return Err(failure(
                "local action CLIENT function did not return an opaque action value",
            ));
        };
        let local_descriptor = decode_action_payload(&active, local_action.canonical_payload())?;
        require(
            local_descriptor.domain() == ActionTargetDomain::Client
                && local_descriptor.target() == local_target
                && local_descriptor.target_revision() == active.pair()
                && local_descriptor.arguments().len() == 1
                && local_descriptor.arguments()[0].parameter() == local_target_parameter
                && local_descriptor.arguments()[0].value() == local_argument.value(),
            "local action value lost its authenticated CLIENT target or canonical argument",
        )?;
        let mut action_state = ClientActionState::default();
        let mut state = ClientStateStore::default();
        let (server, client_stream) = StandardUnixStream::pair()?;
        client_stream.set_nonblocking(true)?;
        let authorizer = RawResourceRequestAuthorizer::new();
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer.clone(),
        ));
        let mut executor =
            orna_server::InstalledClientResourceExecutor::new_with_stream_and_resource_authorizer(
                kernel.clone(),
                session,
                active.clone(),
                client_stream,
                authorizer,
            );
        let action_result = trigger_client_action(
            &active,
            result.value(),
            &authorisation,
            result.context(),
            &mut action_state,
            &[],
            &LocalCapabilityGrantSet::new(),
            &mut state,
            &mut executor,
        );
        let action_result: TestResult<ClientActionOutcome> = match action_result {
            Err(ClientActionError::Pending) => {
                let completion = match timeout(Duration::from_secs(5), async {
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
                {
                    Ok(Ok(completion)) => Ok(completion),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(failure("installed action resource completion timed out")),
                };
                match completion {
                    Ok(completion) => {
                        if !matches!(
                            &completion,
                            ClientResourceCompletion::Ready { value, .. }
                                if value == &RuntimeValue::Integer(43)
                        ) {
                            Err(failure(
                                "installed SERVER action did not return typed INTEGER(43)",
                            ))
                        } else {
                            complete_client_action(
                                &active,
                                &mut action_state,
                                completion,
                                &mut executor,
                            )
                            .map_err(|error| {
                                failure(format!(
                                    "installed action resource completion failed: {error:?}"
                                ))
                            })
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            result => Err(failure(format!(
                "installed SERVER action did not enter its pending executor path: {result:?}"
            ))),
        };
        drop(executor);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let outcome = finish_session(action_result, connection, "installed action socket cleanup")?;
        require(
            outcome == ClientActionOutcome::Completed
                && matches!(action_state.status(), ClientResourceStatus::Idle),
            "authenticated SERVER action did not complete through the installed executor",
        )?;
        let mut local_action_state = ClientActionState::default();
        let mut local_state = ClientStateStore::default();
        let mut local_executor = DeterministicStreamResourceExecutor;
        let local_action_result = trigger_client_action(
            &active,
            local_result.value(),
            &local_authorisation,
            local_result.context(),
            &mut local_action_state,
            &[],
            &LocalCapabilityGrantSet::new(),
            &mut local_state,
            &mut local_executor,
        );
        require(
            local_action_result == Ok(ClientActionOutcome::Completed)
                && matches!(local_action_state.status(), ClientResourceStatus::Idle),
            "local CLIENT action did not complete through the installed executor",
        )?;
        let parent_invocation_id = result.context().parent_invocation_id().to_bytes().to_vec();
        let target_bytes = target.to_bytes().to_vec();
        let payload_call_site_bytes = descriptor.call_site().to_bytes().to_vec();
        let audit_session = database.open().await?;
        let audit_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT parent_invocation_id, nested_invocation_id, request_id,
                            call_site_id, target_function_id, source_revision_id,
                            catalogue_revision_id, decision_outcome, terminal_outcome,
                            item_count, byte_count
                     FROM _orna_kernel.resource_audit_events
                     WHERE parent_invocation_id = $1 AND target_function_id = $2
                     ORDER BY sequence DESC
                     LIMIT 1",
                    &[&parent_invocation_id, &target_bytes],
                )
                .await?;
            let parent: Vec<u8> = row.try_get("parent_invocation_id")?;
            let nested_invocation: Vec<u8> = row.try_get("nested_invocation_id")?;
            let request_id: Vec<u8> = row.try_get("request_id")?;
            let call_site: Vec<u8> = row.try_get("call_site_id")?;
            let audited_target: Vec<u8> = row.try_get("target_function_id")?;
            let source_revision: Vec<u8> = row.try_get("source_revision_id")?;
            let catalogue_revision: Vec<u8> = row.try_get("catalogue_revision_id")?;
            let decision: &str = row.try_get("decision_outcome")?;
            let terminal: &str = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            require(
                parent == parent_invocation_id
                    && nested_invocation.len() == 16
                    && request_id.len() == 16
                    && call_site.len() == 16
                    && call_site != payload_call_site_bytes
                    && audited_target == target_bytes
                    && source_revision == active.pair().source().to_bytes().to_vec()
                    && catalogue_revision == active.pair().catalogue().to_bytes().to_vec()
                    && decision == "allowed"
                    && terminal == "completed"
                    && item_count == Some(1)
                    && byte_count.is_some(),
                "SERVER action did not retain its authenticated redacted resource audit evidence",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "installed action resource audit",
        )
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_server_action_denial_stays_inside_authenticated_resource_trigger() -> TestResult<()>
{
    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_five = install_v5_standard(&kernel, &empty, &database).await?;
        let upgrade_v6 =
            orna_standard::prepare_standard_upgrade_v5_to_v6(&version_five).map_err(|error| {
                failure(format!(
                    "prepare V5-to-V6 standard upgrade failed: {error:?}"
                ))
            })?;
        let active = kernel
            .apply_standard_upgrade(&upgrade_v6)
            .await
            .map_err(|error| {
                failure(format!("apply V5-to-V6 standard upgrade failed: {error:?}"))
            })?;
        let standard_source = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("action denial fixture has no checked standard source"))?;
        let standard = check_standard_library_source(&standard_source).map_err(|error| {
            failure(format!(
                "action denial standard source check failed: {error:?}"
            ))
        })?;
        let (
            active,
            client,
            target,
            client_parameter,
            target_parameter,
            _local_client,
            local_target,
            _local_client_parameter,
            local_target_parameter,
        ) = install_action_client_fixture(&kernel, &active, &standard).await?;
        let mut function_targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .collect::<Vec<_>>();
        function_targets.extend(standard.verified_snapshot().executables().iter().map(
            |executable| {
                SecurityFunctionTarget::verified_standard(
                    executable.function(),
                    standard.verified_snapshot().revision(),
                    executable.revision().id(),
                )
            },
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
                ExecuteGrant::new(RAW_CLIENT_USER, local_target),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let (root_outcome, _, _) = installed_invoke_run(
            &database,
            installed_invoke_request(
                InvocationRequestTarget::function_id(local_target),
                vec![CliArgumentInput::Canonical {
                    parameter: local_target_parameter.canonical(),
                    value: "43".to_owned(),
                }],
                true,
                false,
            ),
        )
        .await?;
        require(
            root_outcome == Ok(InstalledInvokeOutcome::Completed),
            "action denial evaluator could not create an owned root invocation",
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
                    &[
                        &local_target.to_bytes().to_vec(),
                        &RAW_CLIENT_USER.to_bytes().to_vec(),
                    ],
                )
                .await?;
            let bytes: Vec<u8> = row.try_get(0)?;
            let bytes: [u8; 16] = bytes
                .try_into()
                .map_err(|_| {
                    failure("action denial root invocation audit identity was not 16 bytes")
                })?;
            Ok(InvocationId::from_bytes(bytes))
        }
        .await;
        let parent_invocation = finish_session(
            root_operation,
            audit_session.shutdown().await,
            "action denial root invocation audit lookup",
        )?;
        let session = kernel.authenticate_local_peer(uid).await?;
        let authorisation = match security
            .authorise_execute(&session, InvocationTarget::new(client, active.pair()))
        {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "action denial client grant was denied: {denial:?}"
                )));
            }
        };
        let argument = FunctionArgument::new(client_parameter, RuntimeValue::Integer(43))?;
        let evaluation_grants = LocalCapabilityGrantSet::new();
        let mut evaluation_state = ClientStateStore::default();
        let mut evaluation_executor = DeterministicStreamResourceExecutor;
        let result =
            evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorisation,
                std::slice::from_ref(&argument),
                &[],
                &evaluation_grants,
                &mut evaluation_state,
                parent_invocation,
                &mut evaluation_executor,
            )?;
        require(
            result.context().parent_invocation_id() == parent_invocation,
            "action denial evaluator did not retain its authenticated parent invocation",
        )?;
        let RuntimeValue::Opaque(action) = result.value() else {
            return Err(failure(
                "action denial CLIENT function did not return an opaque action value",
            ));
        };
        let descriptor = decode_action_payload(&active, action.canonical_payload())?;
        require(
            descriptor.domain() == ActionTargetDomain::Server
                && descriptor.target() == target
                && descriptor.target_revision() == active.pair()
                && descriptor.arguments().len() == 1
                && descriptor.arguments()[0].parameter() == target_parameter
                && descriptor.arguments()[0].value() == argument.value(),
            "action denial value lost its authenticated SERVER target or canonical argument",
        )?;
        let security_events_before = kernel.recover_security_audit_events().await?;
        let invocation_rows_before = invocation_audit_rows(&database).await?;
        let mut action_state = ClientActionState::default();
        let mut state = ClientStateStore::default();
        let (server, client_stream) = StandardUnixStream::pair()?;
        client_stream.set_nonblocking(true)?;
        let authorizer = RawResourceRequestAuthorizer::new();
        let connection = tokio::spawn(serve_local_raw_stream_with_resource_authorizer(
            kernel.clone(),
            server,
            LocalRawSocketResources::new(),
            authorizer.clone(),
        ));
        let mut executor =
            orna_server::InstalledClientResourceExecutor::new_with_stream_and_resource_authorizer(
                kernel.clone(),
                session,
                active.clone(),
                client_stream,
                authorizer,
            );
        let action_result = trigger_client_action(
            &active,
            result.value(),
            &authorisation,
            result.context(),
            &mut action_state,
            &[],
            &LocalCapabilityGrantSet::new(),
            &mut state,
            &mut executor,
        );
        let action_result =
            finish_pending_client_action(&active, &mut action_state, &mut executor, action_result)
                .await
                .map_err(|error| {
                    failure(format!(
                        "installed action resource completion failed: {error:?}"
                    ))
                });
        drop(executor);
        let connection = connection.await.map_err(Into::into).and_then(|result| {
            result.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
        });
        let outcome = finish_session(action_result, connection, "denied action socket cleanup")?;
        require(
            matches!(
                outcome,
                ClientActionOutcome::Failed { code } if code == "action.failed"
            ) && matches!(action_state.status(), ClientResourceStatus::Idle),
            "denied SERVER action did not fail closed through the installed executor",
        )?;
        let security_events_after = kernel.recover_security_audit_events().await?;
        let appended_security_events = security_events_after
            .get(security_events_before.len()..)
            .unwrap_or_default();
        require(
            appended_security_events
                .iter()
                .filter(|event| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::Execute
                        && decision.outcome() == SecurityAuditOutcome::Denied
                })
                .count()
                == 1
                && security_events_after.last().is_some_and(|event| {
                    let decision = event.decision();
                    decision.kind() == SecurityAuditKind::Execute
                        && decision.outcome() == SecurityAuditOutcome::Denied
                        && decision.target() == Some(InvocationTarget::new(target, active.pair()))
                        && decision.denial()
                            == Some(SecurityAuditDenial::Execute(
                                ExecuteDenial::MissingExecuteGrant,
                            ))
                        && decision.effective_principal().is_none()
                        && decision.authorising_principal().is_none()
                }),
            "denied SERVER action did not append one redacted EXECUTE denial",
        )?;
        let invocation_rows_after = invocation_audit_rows(&database).await?;
        require(
            invocation_rows_after.len() == invocation_rows_before.len(),
            "denied SERVER action fabricated a nested invocation audit",
        )?;
        let parent_invocation_id = result.context().parent_invocation_id().to_bytes().to_vec();
        let audit_session = database.open().await?;
        let audit_operation = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT parent_invocation_id, nested_invocation_id, target_function_id,
                            source_revision_id, catalogue_revision_id, decision_outcome,
                            terminal_outcome, item_count, byte_count,
                            (SELECT count(*)
                               FROM _orna_kernel.invocation_audit_events AS invocation
                              WHERE invocation.invocation_id = resource.nested_invocation_id)
                                AS nested_invocation_count
                     FROM _orna_kernel.resource_audit_events AS resource
                     WHERE parent_invocation_id = $1
                     ORDER BY sequence DESC
                     LIMIT 1",
                    &[&parent_invocation_id],
                )
                .await?;
            let parent: Vec<u8> = row.try_get("parent_invocation_id")?;
            let nested_invocation: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
            let nested_invocation_count: i64 = row.try_get("nested_invocation_count")?;
            let audited_target: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
            let decision: &str = row.try_get("decision_outcome")?;
            let terminal: &str = row.try_get("terminal_outcome")?;
            let item_count: Option<i64> = row.try_get("item_count")?;
            let byte_count: Option<i64> = row.try_get("byte_count")?;
            require(
                parent == parent_invocation_id
                    && nested_invocation.is_none()
                    && nested_invocation_count == 0
                    && audited_target == Some(target.to_bytes().to_vec())
                    && source_revision == Some(active.pair().source().to_bytes().to_vec())
                    && catalogue_revision == Some(active.pair().catalogue().to_bytes().to_vec())
                    && decision == "denied"
                    && terminal == "failed"
                    && item_count.is_none()
                    && byte_count.is_none(),
                "denied SERVER action did not retain its authenticated target identity",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "denied action resource audit",
        )
    })
    .await
}
