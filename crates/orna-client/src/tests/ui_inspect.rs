use super::*;

fn inspect_epoch(high: u8, low: u8) -> super::super::InspectEpochId {
    let mut bytes = [0; 16];
    bytes[..8].fill(high);
    bytes[15] = low;
    super::super::InspectEpochId::from_bytes(bytes)
}

#[test]
fn standard_ui_text_constructor_builds_canonical_value_without_executor() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let context = super::super::ClientExecutionContext {
        pair: active.pair(),
        function: super::super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x91; 16]),
        observer_lineage: None,
    };
    let spec = super::super::standard_ui_constructor_spec(
        &active,
        context,
        super::super::STD_UI_TEXT_RUNTIME_CONTRACT,
    )
    .expect("the V9 standard text constructor is intrinsically recognised");
    let value = super::super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[(
            super::super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("Ready".to_owned()),
        )],
    )
    .expect("pure UI construction does not require an executor");
    let RuntimeValue::Opaque(value) = value else {
        panic!("the constructor returns std.ui.UI");
    };
    let body = super::super::decode_ui_constructor_body(value.canonical_payload())
        .expect("the generated frame is canonical");
    assert_eq!(
        body,
        serde_json::json!({
            "kind": "node",
            "contract": {
                "id": "std.ui.text",
                "name": "std.ui.text",
                "version": "1.0"
            },
            "properties": {
                "text": {"type": "std.types.text", "value": "Ready"}
            },
            "slots": {},
            "actions": {}
        })
    );
}
#[test]
fn standard_ui_constructors_build_all_closed_mappings_and_singleton_nesting() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let text = v9_constructor_value(
        &active,
        super::super::STD_UI_TEXT_FUNCTION_ID,
        super::super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        super::super::STD_UI_TEXT_RUNTIME_CONTRACT,
        vec![(
            super::super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("Ready".to_owned()),
        )],
    );
    let text_body = v9_constructor_body(
        &active,
        super::super::STD_UI_TEXT_FUNCTION_ID,
        super::super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        super::super::STD_UI_TEXT_RUNTIME_CONTRACT,
        vec![(
            super::super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("Ready".to_owned()),
        )],
    );
    assert_eq!(
        text_body,
        serde_json::json!({
            "kind": "node",
            "contract": {"id": "std.ui.text", "name": "std.ui.text", "version": "1.0"},
            "properties": {"text": {"type": "std.types.text", "value": "Ready"}},
            "slots": {},
            "actions": {}
        })
    );

    assert_eq!(
        v9_constructor_body(
            &active,
            super::super::STD_UI_BUTTON_FUNCTION_ID,
            super::super::STD_UI_BUTTON_FUNCTION_REVISION_ID,
            super::super::STD_UI_BUTTON_RUNTIME_CONTRACT,
            vec![
                (
                    super::super::STD_UI_BUTTON_LABEL_PARAMETER_ID,
                    RuntimeValue::Text("Run".to_owned()),
                ),
                (
                    super::super::STD_UI_BUTTON_ENABLED_PARAMETER_ID,
                    RuntimeValue::Boolean(true),
                ),
            ],
        ),
        serde_json::json!({
            "kind": "node",
            "contract": {"id": "std.ui.button", "name": "std.ui.button", "version": "1.0"},
            "properties": {
                "label": {"type": "std.types.text", "value": "Run"},
                "enabled": {"type": "std.types.boolean", "value": true}
            },
            "slots": {},
            "actions": {}
        })
    );
    assert_eq!(
        v9_constructor_body(
            &active,
            super::super::STD_UI_TEXT_INPUT_FUNCTION_ID,
            super::super::STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID,
            super::super::STD_UI_TEXT_INPUT_RUNTIME_CONTRACT,
            vec![
                (
                    super::super::STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
                    RuntimeValue::Text("".to_owned()),
                ),
                (
                    super::super::STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
                    RuntimeValue::Text("Search".to_owned()),
                ),
                (
                    super::super::STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID,
                    RuntimeValue::Boolean(false),
                ),
            ],
        ),
        serde_json::json!({
            "kind": "node",
            "contract": {
                "id": "std.ui.text_input",
                "name": "std.ui.text_input",
                "version": "1.0"
            },
            "properties": {
                "text": {"type": "std.types.text", "value": ""},
                "placeholder": {"type": "std.types.text", "value": "Search"},
                "enabled": {"type": "std.types.boolean", "value": false}
            },
            "slots": {},
            "actions": {}
        })
    );

    for (function, revision, identity, parameter, contract) in [
        (
            super::super::STD_UI_PANEL_FUNCTION_ID,
            super::super::STD_UI_PANEL_FUNCTION_REVISION_ID,
            super::super::STD_UI_PANEL_RUNTIME_CONTRACT,
            super::super::STD_UI_PANEL_CONTENT_PARAMETER_ID,
            "std.ui.panel",
        ),
        (
            super::super::STD_UI_ROW_FUNCTION_ID,
            super::super::STD_UI_ROW_FUNCTION_REVISION_ID,
            super::super::STD_UI_ROW_RUNTIME_CONTRACT,
            super::super::STD_UI_ROW_CONTENT_PARAMETER_ID,
            "std.ui.row",
        ),
        (
            super::super::STD_UI_COLUMN_FUNCTION_ID,
            super::super::STD_UI_COLUMN_FUNCTION_REVISION_ID,
            super::super::STD_UI_COLUMN_RUNTIME_CONTRACT,
            super::super::STD_UI_COLUMN_CONTENT_PARAMETER_ID,
            "std.ui.column",
        ),
        (
            super::super::STD_UI_TABS_FUNCTION_ID,
            super::super::STD_UI_TABS_FUNCTION_REVISION_ID,
            super::super::STD_UI_TABS_RUNTIME_CONTRACT,
            super::super::STD_UI_TABS_CONTENT_PARAMETER_ID,
            "std.ui.tabs",
        ),
    ] {
        assert_eq!(
            v9_constructor_body(
                &active,
                function,
                revision,
                identity,
                vec![(parameter, text.clone())],
            ),
            serde_json::json!({
                "kind": "node",
                "contract": {"id": contract, "name": contract, "version": "1.0"},
                "properties": {},
                "slots": {"content": [text_body.clone()]},
                "actions": {}
            })
        );
    }
}

#[test]
fn standard_ui_constructor_dispatches_without_an_executor() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let context = super::super::ClientExecutionContext {
        pair: active.pair(),
        function: super::super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x94; 16]),
        observer_lineage: None,
    };
    let expression = ClientExpressionNode::ExternalContract {
        identity: super::super::STD_UI_TEXT_RUNTIME_CONTRACT.to_owned(),
    };
    let arguments = [(
        super::super::STD_UI_TEXT_PARAMETER_ID,
        RuntimeValue::Text("headless".to_owned()),
    )];
    let grants = capability::LocalCapabilityGrantSet::default();
    let mut state = ClientStateStore::default();
    let lineage = super::super::ObserverLineage::compatibility(context);
    let mut executor: Option<&mut dyn ClientResourceExecutor> = None;
    let mut locals = super::super::ClientLocalEnvironment::new();
    let mut fuel = super::super::ClientExecutionFuel::new();
    let value = super::super::evaluate_expression_with_fuel(
        &active,
        &expression,
        context,
        &lineage,
        &arguments,
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x95; 16]),
        &mut executor,
        &mut locals,
        &mut fuel,
    )
    .expect("constructor expressions do not require a runtime executor");
    assert!(
        matches!(value, RuntimeValue::Opaque(value) if value.opaque_type() == super::super::STD_UI_TYPE_ID)
    );
}

#[test]
fn standard_ui_constructor_rejects_wrong_order_and_runtime_kinds() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let context = super::super::ClientExecutionContext {
        pair: active.pair(),
        function: super::super::STD_UI_BUTTON_FUNCTION_ID,
        function_revision: super::super::STD_UI_BUTTON_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x96; 16]),
        observer_lineage: None,
    };
    let spec = super::super::standard_ui_constructor_spec(
        &active,
        context,
        super::super::STD_UI_BUTTON_RUNTIME_CONTRACT,
    )
    .expect("the V9 button constructor is intrinsically recognised");
    let wrong_order = super::super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[
            (
                super::super::STD_UI_BUTTON_ENABLED_PARAMETER_ID,
                RuntimeValue::Boolean(true),
            ),
            (
                super::super::STD_UI_BUTTON_LABEL_PARAMETER_ID,
                RuntimeValue::Text("Run".to_owned()),
            ),
        ],
    )
    .unwrap_err();
    assert!(matches!(
        *wrong_order,
        super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::InvalidCall,
            ..
        }
    ));

    let wrong_kind = super::super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[
            (
                super::super::STD_UI_BUTTON_LABEL_PARAMETER_ID,
                RuntimeValue::Boolean(true),
            ),
            (
                super::super::STD_UI_BUTTON_ENABLED_PARAMETER_ID,
                RuntimeValue::Boolean(true),
            ),
        ],
    )
    .unwrap_err();
    assert!(matches!(
        *wrong_kind,
        super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::TypeMismatch,
            ..
        }
    ));
}
#[test]
fn standard_ui_constructor_rejects_malformed_content_frame() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let registry = orna_standard::registered_opaque_codecs(
        active
            .catalogue_hash_context()
            .standard()
            .expect("the V9 fixture pins a standard snapshot"),
    )
    .expect("the V9 fixture has the registered UI codec");
    let mut malformed = super::super::UI_MAGIC.as_bytes().to_vec();
    malformed.extend_from_slice(&1_u32.to_be_bytes());
    malformed.push(b' ');
    assert!(matches!(
        OpaqueValue::new(
            &active,
            &registry,
            super::super::STD_UI_TYPE_ID,
            malformed.clone()
        ),
        Err(super::super::OpaqueValueError::InvalidJsonBody { .. })
    ));
    assert!(matches!(
        super::super::decode_ui_constructor_body(&malformed),
        Err(super::super::OpaqueValueError::InvalidJsonBody { .. })
    ));
}

#[test]
fn standard_ui_constructor_rejects_text_over_runtime_bound() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let context = super::super::ClientExecutionContext {
        pair: active.pair(),
        function: super::super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x97; 16]),
        observer_lineage: None,
    };
    let spec = super::super::standard_ui_constructor_spec(
        &active,
        context,
        super::super::STD_UI_TEXT_RUNTIME_CONTRACT,
    )
    .expect("the V9 text constructor is intrinsically recognised");
    let error = super::super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[(
            super::super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("x".repeat(super::super::CLIENT_MAX_RUNTIME_TEXT_BYTES + 1)),
        )],
    )
    .unwrap_err();
    assert!(matches!(
        *error,
        super::super::ClientExecutionError::InvalidOpaqueValue {
            source: super::super::ClientOpaqueValueError::Value(
                super::super::OpaqueValueError::InvalidFrameLength { .. }
            ),
            ..
        }
    ));
}

#[test]
fn standard_ui_constructor_requires_the_v9_standard_snapshot() {
    let standard = standard_v7();
    let active = empty_version_two_active(&standard);
    let context = super::super::ClientExecutionContext {
        pair: active.pair(),
        function: super::super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x98; 16]),
        observer_lineage: None,
    };
    let spec = super::super::standard_ui_constructor_spec(
        &active,
        context,
        super::super::STD_UI_TEXT_RUNTIME_CONTRACT,
    )
    .expect("identity matching is checked before standard admission");
    let error = super::super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[(
            super::super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("not-v9".to_owned()),
        )],
    )
    .unwrap_err();
    assert!(matches!(
        *error,
        super::super::ClientExecutionError::InvalidOpaqueValue {
            source: super::super::ClientOpaqueValueError::Registry(_),
            ..
        }
    ));
}

#[test]
fn same_string_application_function_remains_generic_external_contract() {
    let active = active_with_application_ui_text_identity();
    let context = super::super::ClientExecutionContext {
        pair: active.pair(),
        function: super::super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x99; 16]),
        observer_lineage: None,
    };
    assert!(
        super::super::standard_ui_constructor_spec(
            &active,
            context,
            super::super::STD_UI_TEXT_RUNTIME_CONTRACT
        )
        .is_none(),
        "application definitions retain precedence over standard intrinsics"
    );
    let expression = ClientExpressionNode::ExternalContract {
        identity: super::super::STD_UI_TEXT_RUNTIME_CONTRACT.to_owned(),
    };
    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
    )
    .with_external_contract(|request| {
        assert_eq!(
            request.identity(),
            super::super::STD_UI_TEXT_RUNTIME_CONTRACT
        );
        Ok(RuntimeValue::Boolean(true))
    });
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let grants = capability::LocalCapabilityGrantSet::default();
    let mut state = ClientStateStore::default();
    let lineage = super::super::ObserverLineage::compatibility(context);
    let mut locals = super::super::ClientLocalEnvironment::new();
    let mut fuel = super::super::ClientExecutionFuel::new();
    let value = super::super::evaluate_expression_with_fuel(
        &active,
        &expression,
        context,
        &lineage,
        &[],
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x9a; 16]),
        &mut executor_slot,
        &mut locals,
        &mut fuel,
    )
    .expect("same-string application contracts still use the generic executor");
    assert_eq!(value, RuntimeValue::Boolean(true));
}

#[test]
fn inspect_executor_default_is_fail_closed() {
    let context = super::super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0x11; 16]),
            CatalogueRevisionId::from_bytes([0x22; 16]),
        ),
        function: FunctionId::from_bytes([0x33; 16]),
        function_revision: FunctionRevisionId::from_bytes([0x44; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x55; 16]),
        observer_lineage: None,
    };
    let operation = super::super::ClientInspectOperation::Snapshot {
        target: RuntimeValue::Boolean(true),
    };
    let request = super::super::ClientInspectRequest::new(context, operation);
    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
    );
    assert_eq!(
        executor.inspect(request),
        Err("inspect.runtime_unavailable".to_owned())
    );
}
#[test]
fn inspect_render_external_contract_dispatches_typed_arguments() {
    let context = super::super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        ),
        function: FunctionId::from_bytes([0x73; 16]),
        function_revision: FunctionRevisionId::from_bytes([0x74; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x75; 16]),
        observer_lineage: None,
    };
    let parameter = ParameterId::from_bytes([0x76; 16]);
    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
    )
    .with_external_contract(move |request| {
        assert_eq!(request.identity(), super::super::INSPECT_RENDER_CONTRACT);
        assert_eq!(request.context(), context);
        assert_eq!(
            request.arguments(),
            &[(parameter, RuntimeValue::Boolean(true))],
        );
        Ok(RuntimeValue::Text("ui".to_owned()))
    });
    let mut optional: Option<&mut dyn super::super::ClientResourceExecutor> = Some(&mut executor);
    assert_eq!(
        super::super::evaluate_external_contract(
            super::super::INSPECT_RENDER_CONTRACT,
            context,
            super::super::ObserverLineage::compatibility(context),
            &[(parameter, RuntimeValue::Boolean(true))],
            &mut optional,
        )
        .unwrap(),
        RuntimeValue::Text("ui".to_owned()),
    );
}

#[test]
fn generic_external_contracts_forward_and_fail_closed_without_executor() {
    let context = super::super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0x81; 16]),
            CatalogueRevisionId::from_bytes([0x82; 16]),
        ),
        function: FunctionId::from_bytes([0x83; 16]),
        function_revision: FunctionRevisionId::from_bytes([0x84; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x85; 16]),
        observer_lineage: None,
    };
    let parameter = ParameterId::from_bytes([0x86; 16]);
    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
    )
    .with_external_contract(move |request| {
        assert_eq!(request.identity(), "app.other@1");
        assert_eq!(
            request.arguments(),
            &[(parameter, RuntimeValue::Boolean(true))]
        );
        Ok(RuntimeValue::Boolean(false))
    });
    let mut optional: Option<&mut dyn super::super::ClientResourceExecutor> = Some(&mut executor);
    assert_eq!(
        super::super::evaluate_external_contract(
            "app.other@1",
            context,
            super::super::ObserverLineage::compatibility(context),
            &[(parameter, RuntimeValue::Boolean(true))],
            &mut optional,
        ),
        Ok(RuntimeValue::Boolean(false)),
    );

    let mut nested_provider = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
    )
    .with_external_contract(|request| {
        assert_eq!(request.identity(), "app.other@1");
        Ok(RuntimeValue::Boolean(true))
    });
    let request = super::super::ClientExternalContractRequest::new(
        context,
        "app.other@1",
        vec![(parameter, RuntimeValue::Boolean(true))],
    );
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut nested_provider,
        pending_request: None,
    };
    assert_eq!(
        nested.external_contract(request),
        Ok(RuntimeValue::Boolean(true)),
        "nested CLIENT actions must retain external-contract providers",
    );

    let mut forwarding_slot: Option<&mut dyn super::super::ClientResourceExecutor> =
        Some(&mut nested);
    assert_eq!(
        super::super::evaluate_external_contract(
            "app.other@1",
            context,
            super::super::ObserverLineage::compatibility(context),
            &[(parameter, RuntimeValue::Boolean(true))],
            &mut forwarding_slot,
        ),
        Ok(RuntimeValue::Boolean(true)),
    );

    let mut failing = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
    )
    .with_external_contract(|_| Err("inspect.denied".to_owned()));
    let mut failing_slot: Option<&mut dyn super::super::ClientResourceExecutor> =
        Some(&mut failing);
    assert!(matches!(
        super::super::evaluate_external_contract(
            "app.other@1",
            context,
            super::super::ObserverLineage::compatibility(context),
            &[],
            &mut failing_slot,
        ),
        Err(super::super::ClientExecutionError::ExternalContract { identity, .. })
            if identity == "app.other@1"
    ));

    let mut default_executor = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
    );
    let mut default_slot: Option<&mut dyn super::super::ClientResourceExecutor> =
        Some(&mut default_executor);
    assert_eq!(
        super::super::evaluate_external_contract(
            super::super::INSPECT_RENDER_CONTRACT,
            context,
            super::super::ObserverLineage::compatibility(context),
            &[],
            &mut default_slot,
        ),
        Err(super::super::ClientExecutionError::Inspect {
            context,
            source: super::super::ClientInspectError::Failed(
                "inspect.runtime_unavailable".to_owned()
            ),
        }),
    );

    let mut absent: Option<&mut dyn super::super::ClientResourceExecutor> = None;
    assert!(matches!(
        super::super::evaluate_external_contract(
            "app.other@1",
            context,
            super::super::ObserverLineage::compatibility(context),
            &[],
            &mut absent,
        ),
        Err(super::super::ClientExecutionError::ExternalContract { identity, .. })
            if identity == "app.other@1"
    ));
    assert_eq!(
        super::super::evaluate_external_contract(
            super::super::INSPECT_RENDER_CONTRACT,
            context,
            super::super::ObserverLineage::compatibility(context),
            &[],
            &mut absent,
        ),
        Err(super::super::ClientExecutionError::Inspect {
            context,
            source: super::super::ClientInspectError::Failed(
                "inspect.runtime_unavailable".to_owned()
            ),
        }),
    );
}
#[test]
fn inspect_render_provider_errors_are_whitelisted_and_redacted() {
    assert_eq!(
        super::super::stable_inspect_provider_error("inspect.denied"),
        "inspect.denied"
    );
    assert_eq!(
        super::super::stable_inspect_provider_error("inspect.revision_mismatch"),
        "inspect.epoch_mismatch"
    );
    assert_eq!(
        super::super::stable_inspect_provider_error("inspect.epoch_unavailable"),
        "inspect.stale_epoch"
    );
    assert_eq!(
        super::super::stable_inspect_provider_error("secret provider detail"),
        "inspect.projection_failed"
    );
    assert_eq!(
        super::super::stable_inspect_provider_error("inspect.projection_failed\0secret"),
        "inspect.projection_failed"
    );
}

#[test]
fn inspect_request_provenance_rejects_observer_target() {
    let context = super::super::ClientExecutionContext {
        pair: super::super::RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0x01; 16]),
            orna_core::CatalogueRevisionId::from_bytes([0x02; 16]),
        ),
        function: super::super::FunctionId::from_bytes([0x03; 16]),
        function_revision: super::super::FunctionRevisionId::from_bytes([0x04; 16]),
        parent_invocation_id: super::super::InvocationId::from_bytes([0x05; 16]),
        observer_lineage: None,
    };
    assert!(super::super::inspect_target_is_observer(
        context,
        context.observer_root_invocation_id(),
    ));
    let request = super::super::ClientInspectRequest::new(
        context,
        super::super::ClientInspectOperation::Snapshot {
            target: super::super::RuntimeValue::Reference {
                target: super::super::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: orna_core::ObjectId::from_bytes([0x06; 16]),
            },
        },
    );
    assert_eq!(
        request.observer_root_invocation_id(),
        context.parent_invocation_id()
    );
    assert_eq!(
        request.observer_parent_invocation_id(),
        context.parent_invocation_id()
    );
    assert_eq!(request.observer_purpose(), "inspect");
    assert_eq!(
        request.target_invocation_id(),
        Some(super::super::InvocationId::from_bytes([0x06; 16]))
    );
}

#[test]
fn nested_observer_lineage_propagates_root_parent_and_current() {
    let root = super::super::InvocationId::from_bytes([0x31; 16]);
    let top = super::super::ObserverLineage::top_level(root);
    assert_eq!(top.root, root);
    assert_eq!(top.parent, root);
    assert_eq!(top.current, root);

    let nested = top.nested();
    assert_eq!(nested.root, root);
    assert_eq!(nested.parent, root);
    assert_ne!(nested.current, root);

    let child = nested.nested();
    assert_eq!(child.root, root);
    assert_eq!(child.parent, nested.current);
    assert_ne!(child.current, nested.current);

    let grandchild = child.nested();
    assert_eq!(grandchild.root, root);
    assert_eq!(grandchild.parent, child.current);
    assert!(grandchild.contains(root));
    assert!(grandchild.contains(nested.current));
    assert!(grandchild.contains(child.current));
    assert!(grandchild.contains(grandchild.current));

    let context = super::super::ClientExecutionContext {
        pair: super::super::RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0x32; 16]),
            orna_core::CatalogueRevisionId::from_bytes([0x33; 16]),
        ),
        function: super::super::FunctionId::from_bytes([0x34; 16]),
        function_revision: super::super::FunctionRevisionId::from_bytes([0x35; 16]),
        parent_invocation_id: nested.current,
        observer_lineage: Some(nested),
    };
    assert_eq!(context.observer_root_invocation_id(), root);
    assert_eq!(context.observer_parent_invocation_id(), nested.current);
    let operation = super::super::ClientInspectOperation::Snapshot {
        target: super::super::RuntimeValue::Boolean(true),
    };
    let compatibility_request = super::super::ClientInspectRequest::new(context, operation.clone());
    assert_eq!(compatibility_request.observer_root_invocation_id(), root);
    assert_eq!(
        compatibility_request.observer_parent_invocation_id(),
        nested.current
    );
    assert_eq!(
        compatibility_request.observer_lineage(),
        &[root, nested.current]
    );
    let external_request =
        super::super::ClientExternalContractRequest::new(context, "app.test@1", Vec::new());
    assert_eq!(external_request.observer_root_invocation_id(), root);
    assert_eq!(
        external_request.observer_parent_invocation_id(),
        nested.current
    );
    let request =
        super::super::ClientInspectRequest::with_provenance(context, operation, None, None, nested);
    assert_eq!(request.observer_root_invocation_id(), root);
    assert_eq!(request.observer_parent_invocation_id(), nested.current);
    assert_eq!(request.observer_lineage(), &[root, nested.current]);

    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
    )
    .with_external_contract(move |request| {
        assert_eq!(request.observer_root_invocation_id(), root);
        assert_eq!(request.observer_parent_invocation_id(), child.current);
        Ok(RuntimeValue::Text("ui".to_owned()))
    });
    let mut executor_slot: Option<&mut dyn super::super::ClientResourceExecutor> =
        Some(&mut executor);
    let value = super::super::evaluate_external_contract(
        super::super::INSPECT_RENDER_CONTRACT,
        context,
        child,
        &[],
        &mut executor_slot,
    )
    .unwrap();
    assert_eq!(value, RuntimeValue::Text("ui".to_owned()));
}

#[test]
fn inspect_snapshot_validation_rejects_empty_carrier_rows() {
    let (active, _, pair, _) = version_one_active(true);
    let envelope = super::super::InspectCarrierEnvelope::new(
        super::super::InspectCarrierKind::Snapshot,
        inspect_epoch(0, 7),
        pair.source(),
        pair.catalogue(),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        super::super::inspect_snapshot_target_from_envelope(&active, &envelope),
        Err(super::super::ClientInspectError::Failed(
            "inspect.malformed_carrier".to_owned(),
        )),
    );
}

#[test]
fn inspect_snapshot_row_binding_preserves_server_root_authority() {
    let target = super::super::InvocationId::from_bytes([0x17; 16]);
    let root_target = super::super::FunctionId::from_bytes([0x18; 16]);
    let mut row = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
    row.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    row.extend_from_slice(&target.to_bytes());
    row.extend_from_slice(&root_target.to_bytes());
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(0);
    row.push(0);
    assert_eq!(
        super::super::decode_inspect_snapshot_target_row(&row, inspect_epoch(0, 7)),
        Ok(target)
    );
    let mut high_half_mismatch = row.clone();
    high_half_mismatch[9] = 1;
    assert_eq!(
        super::super::decode_inspect_snapshot_target_row(&high_half_mismatch, inspect_epoch(0, 7)),
        Err(super::super::ClientInspectError::Failed(
            "inspect.epoch_mismatch".to_owned()
        ))
    );
    let forged_root = super::super::FunctionId::from_bytes([0x19; 16]);
    row[41..57].copy_from_slice(&forged_root.to_bytes());
    assert_eq!(
        super::super::decode_inspect_snapshot_target_row(&row, inspect_epoch(0, 7)),
        Ok(target)
    );
    assert_eq!(
        super::super::decode_inspect_snapshot_target_row(&row, inspect_epoch(0, 8)),
        Err(super::super::ClientInspectError::Failed(
            "inspect.epoch_mismatch".to_owned()
        ))
    );
    row.extend_from_slice(&[0x19; 16]);
    assert_eq!(
        super::super::decode_inspect_snapshot_target_row(&row, inspect_epoch(0, 7)),
        Err(super::super::ClientInspectError::Failed(
            "inspect.malformed_carrier".to_owned()
        ))
    );
}

#[test]
fn inspect_snapshot_row_rejects_zero_value_batch_count() {
    let target = super::super::InvocationId::from_bytes([0x17; 16]);
    let mut row = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
    row.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    row.extend_from_slice(&target.to_bytes());
    row.extend_from_slice(&[0x18; 16]);
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(0);

    assert_eq!(row.len(), 76);
    assert_eq!(
        super::super::decode_inspect_snapshot_target_row(&row, inspect_epoch(0, 7)),
        Err(super::super::ClientInspectError::Failed(
            "inspect.malformed_carrier".to_owned()
        ))
    );
    row[67..75].copy_from_slice(&1_u64.to_be_bytes());
    assert_eq!(
        super::super::decode_inspect_snapshot_target_row(&row, inspect_epoch(0, 7)),
        Ok(target)
    );
    row.push(0x19);
    assert_eq!(
        super::super::decode_inspect_snapshot_target_row(&row, inspect_epoch(0, 7)),
        Err(super::super::ClientInspectError::Failed(
            "inspect.malformed_carrier".to_owned()
        ))
    );
}

#[test]
fn inspect_render_wrong_contract_identity_fails_closed_before_provider() {
    let context = super::super::ClientExecutionContext {
        pair: super::super::RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0x21; 16]),
            orna_core::CatalogueRevisionId::from_bytes([0x22; 16]),
        ),
        function: super::super::FunctionId::from_bytes([0x23; 16]),
        function_revision: super::super::FunctionRevisionId::from_bytes([0x24; 16]),
        parent_invocation_id: super::super::InvocationId::from_bytes([0x25; 16]),
        observer_lineage: None,
    };
    let (active, _, _, _) = version_one_active(true);
    assert!(matches!(
        super::super::validate_inspect_render_contract(&active, context, "std.inspect.render@2", &[]),
        Err(super::super::ClientExecutionError::Inspect {
            source: super::super::ClientInspectError::Failed(code), ..
        }) if code == "inspect.malformed_carrier"
    ));
}

#[test]
fn inspect_render_rejects_mixed_target_before_rendering() {
    let verified = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let active = empty_version_two_active(&verified);
    let pair = active.pair();
    let function = FunctionId::from_bytes([0x90; 16]);
    let function_revision = FunctionRevisionId::from_bytes([0x8f; 16]);
    let target = InvocationId::from_bytes([0x91; 16]);
    let epoch = inspect_epoch(0x96, 7);
    let parameter = ParameterId::from_bytes([0x92; 16]);
    let context = super::super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0x93; 16]),
        observer_lineage: None,
    };
    let encode_row = |payload: Vec<u8>| {
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("standard catalogue");
        let registry = super::super::registered_opaque_codecs(standard).expect("opaque registry");
        let descriptor = super::super::TypeDescriptor::list(super::super::TypeDescriptor::named(
            super::super::BINARY_LARGE_OBJECT_TYPE_ID,
        ))
        .expect("row descriptor");
        let value = super::super::RuntimeValue::list(
            &active,
            descriptor,
            vec![super::super::RuntimeValue::Bytes(payload)],
        )
        .expect("row value");
        orna_protocol::encode_constructed_value(&active, &registry, &value).expect("encoded row")
    };

    let epoch_bytes = epoch.to_bytes();
    let mut snapshot_row = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
    snapshot_row.extend_from_slice(&epoch_bytes);
    snapshot_row.extend_from_slice(&target.to_bytes());
    snapshot_row.extend_from_slice(&[0x94; 16]);
    snapshot_row.push(1);
    snapshot_row.extend_from_slice(&0_u64.to_be_bytes());
    snapshot_row.push(0);
    snapshot_row.push(0);
    let snapshot_bytes = super::super::InspectCarrierEnvelope::new(
        super::super::InspectCarrierKind::Snapshot,
        epoch,
        pair.source(),
        pair.catalogue(),
        vec![encode_row(snapshot_row)],
    )
    .expect("snapshot envelope")
    .encode()
    .expect("snapshot bytes");
    let snapshot = super::super::RuntimeValue::Opaque(
        super::super::OpaqueValue::new_inspect_carrier(
            &active,
            super::super::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            snapshot_bytes,
        )
        .expect("snapshot carrier"),
    );

    let mut mixed_row = vec![3, 0, 0, 0, 0, 0, 0, 0, 0];
    mixed_row.extend_from_slice(&epoch_bytes);
    mixed_row.extend_from_slice(&[0xaa; 16]);
    mixed_row.extend_from_slice(&[0x94; 16]);
    mixed_row.extend_from_slice(&pair.source().to_bytes());
    mixed_row.extend_from_slice(&pair.catalogue().to_bytes());
    mixed_row.push(1);
    mixed_row.push(0);
    let mixed_bytes = super::super::InspectCarrierEnvelope::new(
        super::super::InspectCarrierKind::Calls,
        epoch,
        pair.source(),
        pair.catalogue(),
        vec![encode_row(mixed_row)],
    )
    .expect("mixed projection envelope")
    .encode()
    .expect("mixed projection bytes");
    let mixed = super::super::RuntimeValue::Opaque(
        super::super::OpaqueValue::new_inspect_carrier(
            &active,
            super::super::SYS_INSPECT_CALLS_TYPE_ID,
            mixed_bytes,
        )
        .expect("mixed projection carrier"),
    );

    let expression = orna_artifact::client_plan::ClientExpressionNode::Inspect {
        operation: orna_artifact::client_plan::InspectOperationNode::Projection {
            projection: InspectProjection::Calls,
            snapshot: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead { parameter },
            ),
        },
    };
    let provider_calls = Rc::new(Cell::new(0_u8));
    let provider_calls_for_executor = Rc::clone(&provider_calls);
    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| {
            Ok::<_, String>(super::super::RuntimeValue::Boolean(false))
        },
    )
    .with_inspect(move |_| {
        provider_calls_for_executor.set(provider_calls_for_executor.get() + 1);
        Ok(mixed.clone())
    });
    let mut executor_slot: Option<&mut dyn super::super::ClientResourceExecutor> =
        Some(&mut executor);
    let mut state = super::super::ClientStateStore::new();
    let mut locals = std::collections::HashMap::new();
    let arguments = [(parameter, snapshot)];

    let result = super::super::evaluate_expression_plan(
        &active,
        &expression,
        context,
        super::super::ObserverLineage::compatibility(context),
        ResolvedType::Value(super::super::SYS_INSPECT_CALLS_TYPE_ID),
        &arguments,
        &[],
        &super::super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        PrincipalId::from_bytes([0x95; 16]),
        &mut executor_slot,
        &mut locals,
    );
    assert!(matches!(
        result,
        Err(super::super::ClientExecutionError::Inspect {
            source: super::super::ClientInspectError::Failed(code),
            ..
        }) if code == "inspect.epoch_mismatch"
    ));
    assert_eq!(
        provider_calls.get(),
        1,
        "the mixed carrier came from the custom provider"
    );
}

#[test]
fn inspect_render_wrong_ui_type_fails_closed() {
    let (active, _, _, _) = version_one_active(true);
    assert!(!super::super::inspect_render_ui_value_matches(
        &active,
        &super::super::RuntimeValue::Boolean(false),
    ));
}

#[test]
fn inspector_invocation_references_require_sealed_type_and_nonzero_object() {
    let (active, _, _, _) = version_one_active(true);
    let expected = ResolvedType::Reference {
        target: super::super::SYS_INSPECT_INVOCATION_TYPE_ID,
    };
    assert!(super::super::runtime_value_matches(
        &active,
        &RuntimeValue::Reference {
            target: super::super::SYS_INSPECT_INVOCATION_TYPE_ID,
            object: orna_core::ObjectId::from_bytes([0x11; 16]),
        },
        expected,
    ));
    assert!(!super::super::runtime_value_matches(
        &active,
        &RuntimeValue::Reference {
            target: super::super::SYS_INSPECT_INVOCATION_TYPE_ID,
            object: orna_core::ObjectId::from_bytes([0; 16]),
        },
        expected,
    ));
    assert!(!super::super::runtime_value_matches(
        &active,
        &RuntimeValue::Reference {
            target: TypeId::from_bytes([0x12; 16]),
            object: orna_core::ObjectId::from_bytes([0x11; 16]),
        },
        expected,
    ));
}

#[test]
fn inspector_procedural_local_types_preserve_reference_and_carrier_shapes() {
    let (active, _, _, _) = version_one_active(true);
    assert_eq!(
        super::super::resolve_client_local_type(
            &active,
            super::super::SYS_INSPECT_INVOCATION_TYPE_ID
        ),
        Some(ResolvedType::Reference {
            target: super::super::SYS_INSPECT_INVOCATION_TYPE_ID,
        }),
    );
    assert_eq!(
        super::super::resolve_client_local_type(
            &active,
            super::super::SYS_INSPECT_SNAPSHOT_TYPE_ID
        ),
        Some(ResolvedType::Value(
            super::super::SYS_INSPECT_SNAPSHOT_TYPE_ID
        )),
    );
}

#[test]
fn inspector_carriers_reject_malformed_and_stale_revision_envelopes() {
    let (active, _, pair, _) = version_one_active(true);
    let payload = super::super::InspectCarrierEnvelope::new(
        super::super::InspectCarrierKind::Snapshot,
        inspect_epoch(0, 7),
        pair.source(),
        pair.catalogue(),
        vec![],
    )
    .unwrap()
    .encode()
    .unwrap();
    let value = RuntimeValue::Opaque(
        OpaqueValue::new_inspect_carrier(
            &active,
            super::super::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            payload.clone(),
        )
        .unwrap(),
    );
    assert!(super::super::runtime_value_matches(
        &active,
        &value,
        ResolvedType::Named(super::super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
    ));
    assert!(!super::super::runtime_value_matches(
        &active,
        &RuntimeValue::Bytes(vec![0; 4]),
        ResolvedType::Named(super::super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
    ));

    let stale_payload = super::super::InspectCarrierEnvelope::new(
        super::super::InspectCarrierKind::Snapshot,
        inspect_epoch(0, 7),
        SourceRevisionId::from_bytes([0x91; 16]),
        pair.catalogue(),
        vec![],
    )
    .unwrap()
    .encode()
    .unwrap();
    assert_eq!(
        super::super::decode_inspect_carrier_payload(
            &active,
            &stale_payload,
            super::super::SYS_INSPECT_SNAPSHOT_TYPE_ID,
        )
        .unwrap_err(),
        super::super::ClientInspectError::Failed("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_request_exposes_distinct_client_epoch_anchor() {
    let context = super::super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0xa1; 16]),
            CatalogueRevisionId::from_bytes([0xa2; 16]),
        ),
        function: FunctionId::from_bytes([0xa3; 16]),
        function_revision: FunctionRevisionId::from_bytes([0xa4; 16]),
        parent_invocation_id: InvocationId::from_bytes([0xa5; 16]),
        observer_lineage: None,
    };
    let request = super::super::ClientInspectRequest::new(
        context,
        super::super::ClientInspectOperation::Snapshot {
            target: RuntimeValue::Reference {
                target: super::super::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: orna_core::ObjectId::from_bytes([0xa6; 16]),
            },
        },
    );
    assert_eq!(request.client_epoch_id(), context.client_epoch_id());
    assert_eq!(
        request.client_epoch_id().invocation_id(),
        context.parent_invocation_id()
    );
}

#[test]
fn inspect_executor_forwards_typed_request_to_provider() {
    let context = super::super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0x61; 16]),
            CatalogueRevisionId::from_bytes([0x62; 16]),
        ),
        function: FunctionId::from_bytes([0x63; 16]),
        function_revision: FunctionRevisionId::from_bytes([0x64; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x65; 16]),
        observer_lineage: None,
    };
    let operation = super::super::ClientInspectOperation::Projection {
        projection: InspectProjection::Calls,
        snapshot: RuntimeValue::Boolean(true),
    };
    let request = super::super::ClientInspectRequest::new(context, operation);
    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
    )
    .with_inspect(move |request| {
        assert_eq!(request.context(), context);
        assert_eq!(
            request.operation().projection(),
            Some(InspectProjection::Calls)
        );
        assert_eq!(request.operation().projection_carrier_tag(), Some(3));
        assert!(matches!(
            request.operation().snapshot(),
            Some(RuntimeValue::Boolean(true))
        ));
        Ok(RuntimeValue::Boolean(false))
    });
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };
    assert_eq!(
        nested.inspect(request),
        Ok(RuntimeValue::Boolean(false)),
        "nested CLIENT actions must retain Inspector providers",
    );
}

#[test]
fn inspect_expression_rejects_observer_lineage_targets_before_provider() {
    let (active, function, pair, function_revision) = version_one_active(true);
    let root = InvocationId::from_bytes([0x91; 16]);
    let parent = InvocationId::from_bytes([0x92; 16]);
    let current = InvocationId::from_bytes([0x93; 16]);
    let explicit_lineage =
        super::super::ObserverLineage::top_level(root).with_parent_and_current(parent, current);
    let top_level = super::super::ObserverLineage::top_level(root);
    let nested = top_level.nested();
    let child = nested.nested();
    let cases = [
        ("root", explicit_lineage, root),
        ("parent", explicit_lineage, parent),
        ("current", explicit_lineage, current),
        ("recorded nested descendant", child, nested.current),
    ];

    for (label, lineage, target) in cases {
        let context = super::super::ClientExecutionContext {
            pair,
            function,
            function_revision,
            parent_invocation_id: lineage.parent,
            observer_lineage: None,
        };
        let parameter = ParameterId::from_bytes([0x94; 16]);
        let expression = orna_artifact::client_plan::ClientExpressionNode::Inspect {
            operation: orna_artifact::client_plan::InspectOperationNode::snapshot(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead { parameter },
            ),
        };
        let arguments = [(
            parameter,
            RuntimeValue::Reference {
                target: super::super::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: orna_core::ObjectId::from_bytes(target.to_bytes()),
            },
        )];
        let grants = capability::LocalCapabilityGrantSet::new();
        let mut state = ClientStateStore::new();
        let provider_calls = Rc::new(Cell::new(0));
        let provider_calls_for_executor = Rc::clone(&provider_calls);
        let mut executor = super::super::DeterministicClientResourceExecutor::new(
            |_: &super::super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
        )
        .with_inspect(move |_| {
            provider_calls_for_executor.set(provider_calls_for_executor.get() + 1);
            Ok(RuntimeValue::Boolean(false))
        });
        let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
        let mut locals = std::collections::HashMap::new();

        let result = super::super::evaluate_expression_plan(
            &active,
            &expression,
            context,
            lineage,
            ResolvedType::Value(super::super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
            &arguments,
            &[],
            &grants,
            &mut state,
            0,
            PrincipalId::from_bytes([0x95; 16]),
            &mut executor_slot,
            &mut locals,
        );

        assert_eq!(
            result,
            Err(super::super::ClientExecutionError::Inspect {
                context,
                source: super::super::ClientInspectError::Failed("inspect.recursion".to_owned()),
            }),
            "{label} target must be rejected by the expression evaluator",
        );
        assert_eq!(
            provider_calls.get(),
            0,
            "{label} target must not invoke the Inspector provider",
        );
    }
}

#[test]
fn inspect_snapshot_options_reject_before_evaluating_target_or_options() {
    let (active, function, pair, function_revision) = version_one_active(true);
    let context = super::super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: super::super::InvocationId::from_bytes([0xa7; 16]),
        observer_lineage: None,
    };
    let target = super::super::ParameterId::from_bytes([0xa8; 16]);
    let options = super::super::ParameterId::from_bytes([0xa9; 16]);
    let expression = orna_artifact::client_plan::ClientExpressionNode::Inspect {
        operation: orna_artifact::client_plan::InspectOperationNode::Snapshot {
            target: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: target,
                },
            ),
            options: Some(Box::new(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: options,
                },
            )),
        },
    };
    let mut state = super::super::ClientStateStore::new();
    let mut locals = std::collections::HashMap::new();
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = None;

    let result = super::super::evaluate_expression_plan(
        &active,
        &expression,
        context,
        super::super::ObserverLineage::compatibility(context),
        ResolvedType::Value(super::super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
        &[],
        &[],
        &super::super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        PrincipalId::from_bytes([0xaa; 16]),
        &mut executor_slot,
        &mut locals,
    );

    assert_eq!(
        result,
        Err(super::super::ClientExecutionError::Inspect {
            context,
            source: super::super::ClientInspectError::Failed(
                "inspect.projection_failed".to_owned()
            ),
        }),
        "unsupported snapshot options must be rejected before either expression is evaluated",
    );
}
