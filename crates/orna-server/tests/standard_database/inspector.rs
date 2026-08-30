use super::*;

#[cfg(feature = "test-hooks")]
const RAW_ORDINARY_INSPECTOR_SOURCE: &str =
    include_str!("../fixtures/client_inspector_dogfood.orna");
#[cfg(feature = "test-hooks")]
const RAW_FORGED_INSPECTOR_SOURCE: &str = r#"
CREATE CLIENT FUNCTION inspector_app.forged_renderer(
    p_snapshot sys.inspect.snapshot,
    p_invocation_nodes sys.inspect.invocation_nodes,
    p_calls sys.inspect.calls,
    p_resources sys.inspect.resources,
    p_state_cells sys.inspect.state_cells,
    p_ui_nodes sys.inspect.ui_nodes,
    p_presentation_candidates sys.inspect.presentation_candidates,
    p_runtime_bindings sys.inspect.runtime_bindings,
    p_security_decisions sys.inspect.security_decisions
) RETURNS std.ui.UI IS
BEGIN
    RETURN inspector_app.inspector_renderer(
        p_snapshot => p_snapshot,
        p_invocation_nodes => p_invocation_nodes,
        p_calls => p_calls,
        p_resources => p_resources,
        p_state_cells => p_state_cells,
        p_ui_nodes => p_ui_nodes,
        p_presentation_candidates => p_presentation_candidates,
        p_runtime_bindings => p_runtime_bindings,
        p_security_decisions => p_security_decisions
    );
END;
"#;

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_ordinary_client_inspector_through_installed_evaluator() -> TestResult<()> {
    const CONNECTION_PROTOCOL_MAJOR: u16 = 5;
    const MAX_UI_BODY_BYTES: usize = 64 * 1024;

    with_test_database(|database| async move {
        let uid = nix::unistd::geteuid().as_raw();
        let kernel = kernel(&database)?;
        let (mut active, standard_upgrade, _fixture_client, _fixture_server) =
            install_raw_client_fixture_v4(&kernel).await?;
        let standard = standard_upgrade.checked_standard_library();
        let last_ordinal = active
            .source()
            .units()
            .len()
            .checked_sub(1)
            .ok_or_else(|| failure("ordinary Inspector fixture has no retained source unit"))?;
        let source = SourceBundle::new(active.source().units().iter().enumerate().map(
            |(ordinal, unit)| {
                let content = if ordinal == last_ordinal {
                    format!("{}\n{}\n{}", unit.content(), RAW_ORDINARY_INSPECTOR_SOURCE, RAW_FORGED_INSPECTOR_SOURCE)
                } else {
                    unit.content().to_owned()
                };
                SourceUnit::new(unit.logical_path(), content)
            },
        ))?;
        let context = StandardApplicationCheckContext::try_new(active.catalogue(), standard)?;
        let report = check_standard_application(&source, &context);
        if !report.diagnostics().is_empty() {
            return Err(failure(format!(
                "ordinary Inspector source did not compile: {:?}",
                report.diagnostics()
            )));
        }
        active = kernel
            .apply(&prepare_standard_application(&report, active.pair(), &active)?)
            .await
            .map_err(|error| failure(format!("ordinary Inspector source install failed: {error:?}")))?;
        let inspector_renderer = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["inspector_app", "inspector_renderer"])
            .ok_or_else(|| failure("installed Inspector renderer function is missing"))?;
        require(
            matches!(
                inspector_renderer.return_type(),
                FunctionReturn::Single(ResolvedType::Value(type_id))
                    if *type_id == orna_standard::STD_UI_TYPE_ID
            ),
            "installed Inspector renderer return type did not retain the sealed UI value identity",
        )?;
        let expected_renderer_parameters = [
            ("p_snapshot", SYS_INSPECT_SNAPSHOT_TYPE_ID),
            ("p_invocation_nodes", SYS_INSPECT_INVOCATION_NODES_TYPE_ID),
            ("p_calls", SYS_INSPECT_CALLS_TYPE_ID),
            ("p_resources", SYS_INSPECT_RESOURCES_TYPE_ID),
            ("p_state_cells", SYS_INSPECT_STATE_CELLS_TYPE_ID),
            ("p_ui_nodes", SYS_INSPECT_UI_NODES_TYPE_ID),
            (
                "p_presentation_candidates",
                SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
            ),
            ("p_runtime_bindings", SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID),
            ("p_security_decisions", SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID),
        ];
        require(
            inspector_renderer.parameters().len() == expected_renderer_parameters.len()
                && inspector_renderer
                    .parameters()
                    .iter()
                    .zip(expected_renderer_parameters)
                    .all(|(parameter, (name, type_id))| {
                        parameter.name() == name
                            && parameter.resolved_type() == ResolvedType::Value(type_id)
                    }),
            "installed Inspector renderer parameters did not retain sealed value identities",
        )?;
        let forged_renderer = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["inspector_app", "forged_renderer"])
            .ok_or_else(|| failure("installed forged Inspector renderer function is missing"))?;
        let forged_renderer_id = forged_renderer.id();
        let forged_renderer_parameter_ids = forged_renderer
            .parameters()
            .iter()
            .map(|parameter| parameter.id())
            .collect::<Vec<_>>();

        let inspector = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["inspector_app", "inspector"])
            .ok_or_else(|| failure("installed ordinary Inspector function is missing"))?;
        let inspector_parameter = inspector
            .parameter_by_name("p_target")
            .ok_or_else(|| failure("ordinary Inspector is missing p_target"))?
            .id();
        let inspector = inspector.id();
        let target = active
            .catalogue_hash_context()
            .standard()
            .and_then(|standard| standard.catalogue().function_by_id(STD_INVOKE_ECHO_FUNCTION_ID))
            .ok_or_else(|| failure("installed standard is missing std.invoke.echo"))?;
        let registry = registered_opaque_codecs(
            active
                .catalogue_hash_context()
                .standard()
                .ok_or_else(|| failure("ordinary Inspector has no standard context"))?,
        )?;
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            active
                .catalogue()
                .functions()
                .iter()
                .map(|function| SecurityFunctionTarget::application(function.id()))
                .chain(std::iter::once(SecurityFunctionTarget::verified_standard(
                    target.id(),
                    standard.verified_snapshot().revision(),
                    target.current_revision(),
                )))
                .collect(),
            vec![Principal::new(
                RAW_CLIENT_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(RAW_CLIENT_USER, inspector),
                ExecuteGrant::new(RAW_CLIENT_USER, forged_renderer_id),
                ExecuteGrant::new(RAW_CLIENT_USER, target.id()),
            ],
            vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(RAW_CLIENT_USER, vec![])?;
        let target_request = sealed_echo_request(
            InvocationRequestTarget::function_id(target.id()),
            InvocationParameterSelector::parameter_id(STD_INVOKE_ECHO_PARAMETER_ID),
            41,
        )?;
        let retained = encode_invoke_request(&active, &registry, &target_request)?;
        let target_result = kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
            .await?;
        let target_invocation = require_echo_completion(&target_result, 41)?;
        let target_argument = FunctionArgument::new(
            inspector_parameter,
            RuntimeValue::Reference {
                target: orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: ObjectId::from_bytes(target_invocation.to_bytes()),
            },
        )?;
        let authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(inspector, active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!("ordinary Inspector grant was denied: {denial:?}")))
            }
        };
        let mut executor = RecordingInstalledResourceExecutor {
            inner: InstalledClientResourceExecutor::new(
                kernel.clone(),
                session.clone(),
                active.clone(),
            ),
            execute_count: 0,
            inspect_count: 0,
            poll_count: 0,
            completed_values: Vec::new(),
        };
        // Reuse one enclosing invocation identity so the two runs share a client
        // epoch while each snapshot request receives a fresh server epoch.
        let deterministic_parent = InvocationId::from_bytes([0x58; 16]);
        let grants = LocalCapabilityGrantSet::new();
        let mut state = ClientStateStore::new();
        executor.bind_current_invocation(deterministic_parent);
        let result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorisation,
            std::slice::from_ref(&target_argument),
            &[],
            &grants,
            &mut state,
            deterministic_parent,
            &mut executor,
        )?;
        let RuntimeValue::Opaque(ui) = result.value() else {
            return Err(failure("ordinary Inspector did not return an opaque std.ui.UI value"));
        };
        require(
            ui.opaque_type() == orna_standard::STD_UI_TYPE_ID
                && !matches!(result.value(), RuntimeValue::Boolean(_)),
            "ordinary Inspector returned a Boolean or arbitrary opaque value",
        )?;
        let payload = ui.canonical_payload().to_vec();
        let magic = orna_standard::UI_MAGIC.as_bytes();
        require(
            payload.len() >= magic.len() + 4 && payload.starts_with(magic),
            "ordinary Inspector UI payload did not start with canonical ORNA-UI/1 framing",
        )?;
        let length_start = magic.len();
        let body_length = u32::from_be_bytes(
            payload[length_start..length_start + 4]
                .try_into()
                .map_err(|_| failure("ordinary Inspector UI length prefix was truncated"))?,
        ) as usize;
        require(
            body_length <= MAX_UI_BODY_BYTES
                && payload.len() == length_start + 4 + body_length,
            "ordinary Inspector UI payload length was not bounded and exact",
        )?;
        let body = &payload[length_start + 4..];
        let json: serde_json::Value = serde_json::from_slice(body)
            .map_err(|error| failure(format!("ordinary Inspector UI body was not JSON: {error}")))?;
        require(
            json.is_object(),
            "ordinary Inspector UI body was not a canonical JSON object",
        )?;
        require(
            json.get("kind").and_then(serde_json::Value::as_str) == Some("node"),
            "ordinary Inspector UI kind was not the canonical node shape",
        )?;
        let contract = json
            .get("contract")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| failure("ordinary Inspector UI contract was missing"))?;
        require(
            contract.len() == 3
                && contract.get("id").and_then(serde_json::Value::as_str)
                    == Some("std.ui.window")
                && contract.get("name").and_then(serde_json::Value::as_str)
                    == Some("std.ui.window")
                && contract.get("version").and_then(serde_json::Value::as_str) == Some("1.0"),
            "ordinary Inspector UI contract id, name, or version drifted from ORNA-UI/1",
        )?;
        require(
            json.get("call_site_id") == Some(&serde_json::Value::Null)
                && json.get("function_instance_id") == Some(&serde_json::Value::Null),
            "ordinary Inspector UI call-site identity shape was not canonical",
        )?;
        let key = json
            .get("key")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| failure("ordinary Inspector UI key was missing"))?;
        require(
            key.len() == 2
                && key.get("type").and_then(serde_json::Value::as_str)
                    == Some("std.types.text")
                && key
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| value.starts_with("inspector-")),
            "ordinary Inspector UI key did not retain the canonical text shape",
        )?;
        require(
            json.get("slots")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|object| object.is_empty()),
            "ordinary Inspector UI slots were not the canonical empty object",
        )?;
        require(
            json.get("actions")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|object| object.is_empty()),
            "ordinary Inspector UI actions were not the canonical empty object",
        )?;

        let expected_carrier_kinds = [
            InspectCarrierKind::Snapshot,
            InspectCarrierKind::InvocationNodes,
            InspectCarrierKind::Calls,
            InspectCarrierKind::Resources,
            InspectCarrierKind::StateCells,
            InspectCarrierKind::UiNodes,
            InspectCarrierKind::PresentationCandidates,
            InspectCarrierKind::RuntimeBindings,
            InspectCarrierKind::SecurityDecisions,
        ];
        let expected_row_counts = [1usize, 1, 1, 3, 0, 0, 0, 0, 2];
        require(
            executor.inspect_count == expected_carrier_kinds.len()
                && executor.completed_values.len() == expected_carrier_kinds.len()
                && executor
                    .completed_values
                    .iter()
                    .zip(expected_carrier_kinds.iter())
                    .all(|((expected_type, value), expected_kind)| {
                        *expected_type == ResolvedType::Value(expected_kind.type_id())
                            && matches!(
                                value,
                                RuntimeValue::Opaque(value)
                                    if value.opaque_type() == expected_kind.type_id()
                            )
                    }),
            "ordinary Inspector did not deliver the complete ordered nine-carrier set to the renderer",
        )?;
        let mut shared_server_epoch = None;
        let mut observed_row_counts = Vec::with_capacity(expected_carrier_kinds.len());
        for (((expected_type, value), expected_kind), expected_rows) in executor
            .completed_values
            .iter()
            .zip(expected_carrier_kinds.iter())
            .zip(expected_row_counts)
        {
            require(
                *expected_type == ResolvedType::Value(expected_kind.type_id()),
                "ordinary Inspector carrier result type drifted from its sealed identity",
            )?;
            let RuntimeValue::Opaque(value) = value else {
                return Err(failure("ordinary Inspector carrier result was not opaque"));
            };
            let envelope = InspectCarrierEnvelope::decode(value.canonical_payload())
                .map_err(|error| failure(format!("ordinary Inspector carrier envelope was invalid: {error}")))?;
            require(
                envelope.carrier_kind() == *expected_kind
                    && envelope.source_revision_id() == active.pair().source()
                    && envelope.catalogue_revision_id() == active.pair().catalogue()
                    && envelope.rows().len() == expected_rows,
                "ordinary Inspector carrier lost its kind, active revisions, or fixture row count",
            )?;
            if let Some(expected_epoch) = shared_server_epoch {
                require(
                    envelope.server_epoch_id() == expected_epoch,
                    "ordinary Inspector carriers did not share one server epoch",
                )?;
            } else {
                shared_server_epoch = Some(envelope.server_epoch_id());
            }
            observed_row_counts.push(envelope.rows().len());
        }
        let shared_server_epoch =
            shared_server_epoch.ok_or_else(|| failure("ordinary Inspector produced no server epoch"))?;
        require(
            observed_row_counts == expected_row_counts,
            "ordinary Inspector carrier row counts were not deterministic for the echo fixture",
        )?;
        let properties = json
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| failure("ordinary Inspector UI properties were missing"))?;
        let ui_server_epoch = properties
            .get("server_epoch")
            .and_then(|property| property.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("ordinary Inspector UI server_epoch property was missing"))?;
        require(
            ui_server_epoch == shared_server_epoch.to_string(),
            "ordinary Inspector UI server_epoch did not match the shared carrier epoch",
        )?;
        let ui_client_epoch = properties
            .get("client_epoch")
            .and_then(|property| property.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("ordinary Inspector UI client_epoch property was missing"))?;
        require(
            ui_client_epoch == result.context().client_epoch_id().invocation_id().to_string(),
            "ordinary Inspector UI client_epoch did not match the evaluated request context",
        )?;
        let ui_carrier_rows = properties
            .get("carrier_rows")
            .and_then(|property| property.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("ordinary Inspector UI carrier_rows property was missing"))?;
        require(
            ui_carrier_rows == "1,1,1,3,0,0,0,0,2",
            "ordinary Inspector UI carrier_rows did not match the echo fixture",
        )?;
        // The installed evaluator returns the canonical ORNA-UI/1 value. The
        // private headless runtime fixture is covered by orna-client's own
        // `#[cfg(test)]` conformance suite; this installed proof does not
        // expose that fixture through a normal dependency feature.

        let first_carriers = executor.completed_values.clone();
        let forged_arguments = forged_renderer_parameter_ids
            .iter()
            .zip(first_carriers.iter())
            .map(|(parameter, (_, value))| FunctionArgument::new(*parameter, value.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let forged_authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(forged_renderer_id, active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "forged Inspector renderer grant was denied: {denial:?}"
                )))
            }
        };
        let mut forged_state = ClientStateStore::new();
        let forged_result =
            evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &forged_authorisation,
                &forged_arguments,
                &[],
                &grants,
                &mut forged_state,
                deterministic_parent,
                &mut executor,
            )?;
        require(
            matches!(forged_result.value(), RuntimeValue::Opaque(value) if value.opaque_type() == orna_standard::STD_UI_TYPE_ID),
            "normal same-signature renderer did not retain the accepted external UI path",
        )?;
        let forged_contract_arguments = forged_renderer_parameter_ids
            .iter()
            .zip(first_carriers.iter())
            .map(|(parameter, (_, value))| (*parameter, value.clone()))
            .collect::<Vec<_>>();
        let forged_request = ClientExternalContractRequest::new(
            *forged_result.context(),
            INSPECT_RENDER_CONTRACT,
            forged_contract_arguments,
        );
        require(
            executor.inner.external_contract(forged_request)
                == Err("inspect.malformed_carrier".to_owned()),
            "normal same-signature renderer context obtained ORNA-UI from valid carriers",
        )?;
        let mut second_executor = RecordingInstalledResourceExecutor {
            inner: InstalledClientResourceExecutor::new(kernel.clone(), session, active.clone()),
            execute_count: 0,
            inspect_count: 0,
            poll_count: 0,
            completed_values: Vec::new(),
        };
        let mut second_state = ClientStateStore::new();
        second_executor.bind_current_invocation(deterministic_parent);
        let second_result = evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorisation,
            std::slice::from_ref(&target_argument),
            &[],
            &grants,
            &mut second_state,
            deterministic_parent,
            &mut second_executor,
        )?;
        let RuntimeValue::Opaque(second_ui) = second_result.value() else {
            return Err(failure("ordinary Inspector repeat did not return an opaque std.ui.UI value"));
        };
        require(
            second_ui.opaque_type() == orna_standard::STD_UI_TYPE_ID,
            "ordinary Inspector repeat did not return an opaque std.ui.UI value",
        )?;
        require(
            second_executor.completed_values.len() == expected_carrier_kinds.len(),
            "ordinary Inspector repeat did not deliver the complete carrier set",
        )?;
        let mut second_server_epoch = None;
        for (((expected_type, value), expected_kind), expected_rows) in second_executor
            .completed_values
            .iter()
            .zip(expected_carrier_kinds.iter())
            .zip(expected_row_counts)
        {
            require(
                *expected_type == ResolvedType::Value(expected_kind.type_id()),
                "ordinary Inspector repeat carrier type drifted from its sealed identity",
            )?;
            let RuntimeValue::Opaque(value) = value else {
                return Err(failure("ordinary Inspector repeat carrier was not opaque"));
            };
            let envelope = InspectCarrierEnvelope::decode(value.canonical_payload())
                .map_err(|error| {
                    failure(format!(
                        "ordinary Inspector repeat carrier envelope was invalid: {error}"
                    ))
                })?;
            require(
                envelope.carrier_kind() == *expected_kind
                    && envelope.source_revision_id() == active.pair().source()
                    && envelope.catalogue_revision_id() == active.pair().catalogue()
                    && envelope.rows().len() == expected_rows,
                "ordinary Inspector repeat carrier lost its kind, revisions, or row count",
            )?;
            if let Some(expected_epoch) = second_server_epoch {
                require(
                    envelope.server_epoch_id() == expected_epoch,
                    "ordinary Inspector repeat carriers did not share one server epoch",
                )?;
            } else {
                second_server_epoch = Some(envelope.server_epoch_id());
            }
        }
        let second_server_epoch =
            second_server_epoch.ok_or_else(|| failure("ordinary Inspector repeat had no epoch"))?;
        require(
            second_server_epoch != shared_server_epoch,
            "repeated Inspector snapshots reused the previous immutable server epoch",
        )?;
        let second_payload = second_ui.canonical_payload();
        let second_prefix_length = orna_standard::UI_MAGIC.len() + 4;
        require(
            second_payload.len() >= second_prefix_length,
            "ordinary Inspector repeat UI length prefix was truncated",
        )?;
        let second_body_length = u32::from_be_bytes(
            second_payload[orna_standard::UI_MAGIC.len()..second_prefix_length]
                .try_into()
                .map_err(|_| failure("ordinary Inspector repeat UI length was truncated"))?,
        ) as usize;
        require(
            second_payload.len() == second_prefix_length + second_body_length,
            "ordinary Inspector repeat UI framing was not exact",
        )?;
        let second_json: serde_json::Value =
            serde_json::from_slice(&second_payload[second_prefix_length..])
                .map_err(|error| failure(format!("ordinary Inspector repeat UI was not JSON: {error}")))?;
        let second_ui_server_epoch = second_json
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .and_then(|properties| properties.get("server_epoch"))
            .and_then(|property| property.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("ordinary Inspector repeat server_epoch property was missing"))?;
        require(
            second_ui_server_epoch == second_server_epoch.to_string()
                && second_ui_server_epoch != ui_server_epoch,
            "ordinary Inspector repeat UI did not expose its fresh server epoch",
        )?;

        let unavailable = evaluate_client_function_with_arguments(
            &active,
            &authorisation,
            std::slice::from_ref(&target_argument),
        );
        require(
            matches!(
                unavailable,
                Err(ClientExecutionError::Inspect {
                    source: ClientInspectError::Failed(code),
                    ..
                }) if code == "inspect.runtime_unavailable"
            ),
            "ordinary Inspector without an executor did not fail closed",
        )
    })
    .await
}
