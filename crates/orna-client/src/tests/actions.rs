use super::*;

#[test]
fn action_trigger_rejects_domain_mismatch_and_stale_revision() {
    let (active, parent_function, pair, parent_revision, parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0xf6; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None);

    let domain_mismatch = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xf7; 16]),
        vec![argument.clone()],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    assert_eq!(
        trigger_client_action(
            &active,
            &domain_mismatch,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::TargetMismatch),
    );

    let stale_pair = RevisionPair::new(SourceRevisionId::from_bytes([0xf8; 16]), pair.catalogue());
    let stale_target_revision = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        stale_pair,
        CallSiteId::from_bytes([0xf9; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    assert_eq!(
        trigger_client_action(
            &active,
            &stale_target_revision,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::RevisionMismatch),
    );
}

#[test]
fn action_trigger_rejects_wrong_result_type_and_non_single_column_target() {
    let (active, parent_function, pair, parent_revision, parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0xfa; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let wrong_type = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        CallSiteId::from_bytes([0xfb; 16]),
        vec![argument],
        orna_standard::INTEGER_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None);
    assert_eq!(
        trigger_client_action(
            &active,
            &wrong_type,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::ResultTypeMismatch),
    );

    let (multi_column_active, multi_column_function, multi_column_pair, multi_column_revision) =
        version_two_server_rows_active();
    let multi_column_auth = authorise(multi_column_pair, multi_column_function);
    let multi_column_parent = ClientExecutionContext {
        pair: multi_column_pair,
        function: multi_column_function,
        function_revision: multi_column_revision,
        parent_invocation_id: InvocationId::from_bytes([0xfc; 16]),
        observer_lineage: None,
    };
    let multi_column_action = action_value(
        &multi_column_active,
        ActionTargetDomain::Server,
        multi_column_function,
        multi_column_pair,
        CallSiteId::from_bytes([0xfd; 16]),
        Vec::new(),
        orna_standard::BOOLEAN_TYPE_ID,
    );
    let mut multi_column_state = ClientStateStore::default();
    let mut multi_column_action_state = ClientActionState::default();
    let mut multi_column_executor = RecordingActionExecutor::new(None);
    assert_eq!(
        trigger_client_action(
            &multi_column_active,
            &multi_column_action,
            &multi_column_auth,
            &multi_column_parent,
            &mut multi_column_action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut multi_column_state,
            &mut multi_column_executor,
        ),
        Err(ClientActionError::ResultTypeMismatch),
    );
}

#[test]
fn action_target_result_type_rejects_one_column_rows() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::Named(TypeId::from_bytes([0x66; 16])),
        )]),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Server,
        function,
        pair,
        CallSiteId::from_bytes([0xfe; 16]),
        Vec::new(),
        TypeId::from_bytes([0x66; 16]),
    );

    assert_eq!(
        action_target_result_type(&active, &descriptor),
        Err(ClientActionError::ResultTypeMismatch)
    );
}
#[test]
fn action_target_result_type_rejects_stream_targets() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Stream(ResolvedType::Scalar(StandardScalar::Integer)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Server,
        function,
        pair,
        CallSiteId::from_bytes([0x70; 16]),
        Vec::new(),
        orna_standard::INTEGER_TYPE_ID,
    );

    assert_eq!(
        action_target_result_type(&active, &descriptor),
        Err(ClientActionError::ResultTypeMismatch)
    );
}

#[test]
fn action_payload_rejects_malformed_and_noncanonical_frames() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let parameter = ParameterId::from_bytes([0x71; 16]);
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x72; 16]),
        vec![FunctionArgument::new(parameter, RuntimeValue::Integer(7)).unwrap()],
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let magic_length = super::super::ACTION_MAGIC.len();
    let body_offset = magic_length + 4;
    let metadata_length = 1 + (16 * 5);
    let count_offset = body_offset + metadata_length;
    let first_parameter_offset = count_offset + 4;
    let frame_length_offset = first_parameter_offset + 16;
    let frame_offset = frame_length_offset + 4;

    let mut invalid_magic = payload.clone();
    invalid_magic[0] ^= 0xff;

    let mut truncated = payload.clone();
    truncated.pop();

    let mut invalid_count = payload.clone();
    invalid_count[count_offset..count_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());

    let two_argument_descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x73; 16]),
        vec![
            FunctionArgument::new(ParameterId::from_bytes([1; 16]), RuntimeValue::Integer(1))
                .unwrap(),
            FunctionArgument::new(ParameterId::from_bytes([2; 16]), RuntimeValue::Integer(2))
                .unwrap(),
        ],
        orna_standard::INTEGER_TYPE_ID,
    );
    let two_argument_payload = encode_action_payload(&active, &two_argument_descriptor).unwrap();
    let first_two_argument_offset = first_parameter_offset;
    let first_frame_length = u32::from_be_bytes(
        two_argument_payload[first_two_argument_offset + 16..first_two_argument_offset + 20]
            .try_into()
            .unwrap(),
    ) as usize;
    let second_parameter_offset = first_two_argument_offset + 16 + 4 + first_frame_length;
    let mut invalid_order = two_argument_payload;
    invalid_order[second_parameter_offset..second_parameter_offset + 16].copy_from_slice(&[0; 16]);

    let mut trailing = payload.clone();
    trailing.push(0xaa);
    let body_length =
        u32::from_be_bytes(trailing[magic_length..magic_length + 4].try_into().unwrap());
    trailing[magic_length..magic_length + 4].copy_from_slice(&(body_length + 1).to_be_bytes());

    let mut invalid_orv3_frame = payload;
    invalid_orv3_frame[frame_offset..frame_offset + 4].copy_from_slice(b"ORV2");

    for malformed in [
        invalid_magic,
        truncated,
        invalid_count,
        invalid_order,
        trailing,
        invalid_orv3_frame,
    ] {
        assert!(matches!(
            decode_action_payload(&active, &malformed),
            Err(ClientActionError::InvalidPayload(_))
        ));
    }
}

#[test]
fn action_payload_encode_rejects_zero_identity_fields() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let make = |target, source, catalogue, call_site, result_type, parameter| {
        ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            FunctionId::from_bytes(target),
            RevisionPair::new(
                SourceRevisionId::from_bytes(source),
                CatalogueRevisionId::from_bytes(catalogue),
            ),
            CallSiteId::from_bytes(call_site),
            vec![
                FunctionArgument::new(ParameterId::from_bytes(parameter), RuntimeValue::Integer(7))
                    .unwrap(),
            ],
            TypeId::from_bytes(result_type),
        )
    };
    let cases = [
        (
            [0; 16],
            pair.source().to_bytes(),
            pair.catalogue().to_bytes(),
            [0x82; 16],
            [0x44; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            [0; 16],
            pair.catalogue().to_bytes(),
            [0x82; 16],
            [0x44; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            pair.source().to_bytes(),
            [0; 16],
            [0x82; 16],
            [0x44; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            pair.source().to_bytes(),
            pair.catalogue().to_bytes(),
            [0; 16],
            [0x44; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            pair.source().to_bytes(),
            pair.catalogue().to_bytes(),
            [0x82; 16],
            [0; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            pair.source().to_bytes(),
            pair.catalogue().to_bytes(),
            [0x82; 16],
            [0x44; 16],
            [0; 16],
        ),
    ];
    for (target, source, catalogue, call_site, result_type, parameter) in cases {
        assert_eq!(
            encode_action_payload(
                &active,
                &make(target, source, catalogue, call_site, result_type, parameter)
            ),
            Err(ClientActionError::InvalidPayload(
                "invalid action identity".to_owned()
            )),
        );
    }
}

#[test]
fn action_payload_decode_rejects_zero_identity_fields_before_descriptor_construction() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x82; 16]),
        vec![
            FunctionArgument::new(
                ParameterId::from_bytes([0x83; 16]),
                RuntimeValue::Integer(7),
            )
            .unwrap(),
        ],
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let body_offset = super::super::ACTION_MAGIC.len() + 4;
    for relative_offset in [1, 17, 33, 49, 65, 85] {
        let mut corrupted = payload.clone();
        corrupted[body_offset + relative_offset..body_offset + relative_offset + 16].fill(0);
        assert_eq!(
            decode_action_payload(&active, &corrupted),
            Err(ClientActionError::InvalidPayload(
                "invalid action identity".to_owned()
            )),
            "identity field at offset {relative_offset} must be rejected"
        );
    }
}

#[test]
fn action_payload_encodes_multiple_arguments_in_parameter_order_and_round_trips() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x74; 16]),
        vec![
            FunctionArgument::new(ParameterId::from_bytes([1; 16]), RuntimeValue::Integer(11))
                .unwrap(),
            FunctionArgument::new(ParameterId::from_bytes([2; 16]), RuntimeValue::Integer(22))
                .unwrap(),
        ],
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let body_offset = super::super::ACTION_MAGIC.len() + 4;
    let first_parameter_offset = body_offset + 1 + (16 * 5) + 4;
    let first_frame_length = u32::from_be_bytes(
        payload[first_parameter_offset + 16..first_parameter_offset + 20]
            .try_into()
            .unwrap(),
    ) as usize;
    let second_parameter_offset = first_parameter_offset + 16 + 4 + first_frame_length;
    assert_eq!(
        &payload[first_parameter_offset..first_parameter_offset + 16],
        &[1; 16]
    );
    assert_eq!(
        &payload[second_parameter_offset..second_parameter_offset + 16],
        &[2; 16]
    );

    let decoded = decode_action_payload(&active, &payload).unwrap();
    assert_eq!(decoded, descriptor);
    assert_eq!(encode_action_payload(&active, &decoded).unwrap(), payload);
}

#[test]
fn action_trigger_rejects_repeated_pending_server_request_without_mutating_generation() {
    let (active, parent_function, pair, parent_revision, _parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let observer_root = InvocationId::from_bytes([0xfb; 16]);
    let observer_parent = InvocationId::from_bytes([0xfa; 16]);
    let observer_current = InvocationId::from_bytes([0xf9; 16]);
    let observer_lineage = super::super::ObserverLineage::top_level(observer_root)
        .with_parent_and_current(observer_parent, observer_current);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0xfe; 16]),
        observer_lineage: Some(observer_lineage),
    };
    assert_eq!(parent.observer_root_invocation_id(), observer_root);
    assert_eq!(parent.observer_parent_invocation_id(), observer_current);
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        CallSiteId::from_bytes([0xff; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None);

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::Pending),
    );
    let first_request = executor.executed[0].clone();
    assert_eq!(
        first_request
            .invocation_context()
            .expect("server action carries observer provenance")
            .parent_invocation_id(),
        observer_current
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Loading);

    // The one-active-action contract rejects a repeated trigger while loading.
    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::Pending),
    );
    assert_eq!(executor.executed.len(), 1);
    assert_eq!(
        action_state.invocation_id(),
        Some(first_request.request_id())
    );
    assert_eq!(action_state.generation(), Some(first_request.generation()));
    assert_eq!(action_state.status(), ClientResourceStatus::Loading);
}

#[test]
fn action_trigger_after_terminal_completion_allocates_fresh_request_identity() {
    let (active, parent_function, pair, parent_revision, _parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0x01; 16]),
        observer_lineage: None,
    };
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        CallSiteId::from_bytes([0xff; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("completed".to_owned())));

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed),
    );
    let first_request = executor.executed[0].clone();
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(action_state.invocation_id(), None);
    assert_eq!(action_state.generation(), None);

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed),
    );
    assert_eq!(executor.executed.len(), 2);
    let second_request = executor.executed[1].clone();
    assert_ne!(first_request.request_id(), second_request.request_id());
    assert!(second_request.generation().value() > first_request.generation().value());
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_trigger_redacts_executor_failure() {
    let (active, parent_function, pair, parent_revision, _parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0x01; 16]),
        observer_lineage: None,
    };
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        CallSiteId::from_bytes([0x02; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = FailingActionExecutor::default();

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Failed {
            code: ACTION_FAILURE_CODE.to_owned(),
        }),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_payload_round_trip_and_rejects_trailing_bytes() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let parameter = ParameterId::from_bytes([0x71; 16]);
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x72; 16]),
        vec![FunctionArgument::new(parameter, RuntimeValue::Integer(7)).unwrap()],
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    assert_eq!(
        decode_action_payload(&active, &payload).unwrap(),
        descriptor
    );
    let mut trailing = payload.clone();
    trailing.push(0);
    assert!(decode_action_payload(&active, &trailing).is_err());
    let stale_descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        RevisionPair::new(SourceRevisionId::from_bytes([0x73; 16]), pair.catalogue()),
        CallSiteId::from_bytes([0x74; 16]),
        vec![FunctionArgument::new(parameter, RuntimeValue::Integer(7)).unwrap()],
        orna_standard::INTEGER_TYPE_ID,
    );
    let stale_payload = encode_action_payload(&active, &stale_descriptor).unwrap();
    assert_eq!(
        decode_action_payload(&active, &stale_payload),
        Err(ClientActionError::RevisionMismatch),
    );
}

#[test]
fn action_pending_completion_retains_generation_and_redacts_failure() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let generation = request.generation();
    let request_id = request.request_id();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    let mut executor = RecordingActionExecutor::new(None);
    assert_eq!(
        complete_client_action(&active, &mut action_state, request.pending(), &mut executor),
        Err(ClientActionError::Pending)
    );
    assert_eq!(action_state.generation(), Some(generation));
    let failed = ClientResourceCompletion::Failed {
        request_id,
        key,
        generation,
        code: "secret.internal.detail".to_owned(),
    };
    assert_eq!(
        complete_client_action(&active, &mut action_state, failed, &mut executor),
        Ok(ClientActionOutcome::Failed {
            code: ACTION_FAILURE_CODE.to_owned()
        })
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_completed_terminal_rejects_later_same_generation_failed_completion() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    let mut executor = RecordingActionExecutor::new(None);

    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.clone().ready(RuntimeValue::Boolean(true)),
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed)
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    let terminal_state = action_state.clone();
    let terminal_executor = executor.cancelled.clone();

    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.clone().failed("late.failure".to_owned()),
            &mut executor,
        ),
        Err(ClientActionError::StaleCompletion)
    );
    assert_eq!(action_state, terminal_state);
    assert_eq!(executor.cancelled, terminal_executor);
    assert!(executor.cancelled.is_empty());
}

#[test]
fn action_failed_terminal_rejects_later_same_generation_completed_completion() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    let mut executor = RecordingActionExecutor::new(None);

    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.clone().failed("first.failure".to_owned()),
            &mut executor,
        ),
        Ok(ClientActionOutcome::Failed {
            code: ACTION_FAILURE_CODE.to_owned(),
        })
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    let terminal_state = action_state.clone();
    let terminal_executor = executor.cancelled.clone();

    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.clone().ready(RuntimeValue::Boolean(true)),
            &mut executor,
        ),
        Err(ClientActionError::StaleCompletion)
    );
    assert_eq!(action_state, terminal_state);
    assert_eq!(executor.cancelled, terminal_executor);
    assert!(executor.cancelled.is_empty());
}

#[test]
fn action_cancellation_uses_executor_and_rejects_late_completion() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    action_state.stage_invocation(request.request_id());
    action_state.stage_request(request.clone());
    assert_eq!(action_state.invocation_id(), Some(request.request_id()));
    let mut executor = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Boolean(true))
    });

    assert_eq!(
        super::super::cancel_client_action_with_executor(&active, &mut action_state, &mut executor,),
        Ok(ClientActionOutcome::Cancelled),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.ready(RuntimeValue::Boolean(true)),
            &mut executor
        ),
        Err(ClientActionError::StaleCompletion),
    );
    assert_eq!(action_state.generation(), None);
}

#[test]
fn action_trigger_rejects_non_action_values() {
    let (active, function, pair, revision) = version_one_active(true);
    let auth = authorise(pair, function);
    let parent = ClientExecutionContext {
        pair,
        function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf1; 16]),
        observer_lineage: None,
    };
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Boolean(true))
    });
    assert_eq!(
        trigger_client_action(
            &active,
            &RuntimeValue::Boolean(true),
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor
        ),
        Err(super::super::ClientActionError::InvalidValue)
    );
}
#[test]
fn action_current_generation_mismatched_request_is_stale_but_same_request_malformed_completion_cancels()
 {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let wrong_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7b; 16]),
        digest,
        active.catalogue_hash(),
    );

    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let generation = request.generation();
    let mut stale_state = ClientActionState::default();
    stale_state.set_resource(resource);
    let mut stale_executor = RecordingActionExecutor::new(None);
    assert_eq!(
        complete_client_action(
            &active,
            &mut stale_state,
            ClientResourceCompletion::Ready {
                request_id: request.request_id(),
                key: wrong_key,
                generation,
                value: RuntimeValue::Boolean(true),
            },
            &mut stale_executor,
        ),
        Err(ClientActionError::StaleCompletion),
    );
    assert_eq!(stale_state.status(), ClientResourceStatus::Loading);
    assert!(stale_executor.cancelled.is_empty());
    assert_eq!(
        complete_client_action(
            &active,
            &mut stale_state,
            ClientResourceCompletion::Ready {
                request_id: request.request_id(),
                key,
                generation,
                value: RuntimeValue::Integer(1),
            },
            &mut stale_executor,
        ),
        Ok(ClientActionOutcome::Cancelled),
    );
    assert_eq!(stale_state.status(), ClientResourceStatus::Idle);
    assert_eq!(stale_executor.cancelled, vec![request]);

    for malformed_kind in [0_u8, 1_u8] {
        let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let request = resource.begin_request(&active, vec![]).unwrap();
        assert_eq!(request.generation(), generation);
        let completion = if malformed_kind == 0 {
            ClientResourceCompletion::Ready {
                request_id: request.request_id(),
                key,
                generation,
                value: RuntimeValue::Integer(1),
            }
        } else {
            ClientResourceCompletion::Failed {
                request_id: request.request_id(),
                key,
                generation,
                code: String::new(),
            }
        };
        let mut action_state = ClientActionState::default();
        action_state.set_resource(resource);
        action_state.stage_request(request.clone());
        let mut executor = RecordingActionExecutor::new(None);
        assert_eq!(
            complete_client_action(&active, &mut action_state, completion, &mut executor),
            Ok(ClientActionOutcome::Cancelled),
        );
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
        assert_eq!(executor.cancelled, vec![request]);
    }
}

#[test]
fn action_uncertain_cancel_retains_loading_request() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let generation = request.generation();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    action_state.stage_request(request.clone());
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    executor.pending = Some(request.clone());
    let malformed = request.clone().ready(RuntimeValue::Integer(1));

    assert_eq!(
        complete_client_action(&active, &mut action_state, malformed, &mut executor),
        Err(ClientActionError::Pending),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Loading);
    assert_eq!(action_state.generation(), Some(generation));
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.pending, Some(request));
}

#[test]
fn action_malformed_terminal_cancellation_marks_released_request_cancelled() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let generation = request.generation();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    action_state.stage_request(request.clone());
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Integer(7));
    executor.pending = Some(request.clone());
    let malformed = request.clone().ready(RuntimeValue::Integer(1));

    assert_eq!(
        complete_client_action(&active, &mut action_state, malformed, &mut executor),
        Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned())),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Cancelled);
    assert_eq!(action_state.generation(), Some(generation));
    assert_eq!(executor.cancelled, vec![request]);
    assert!(executor.pending.is_none());
}

#[test]
fn nested_action_pending_cancel_retains_pending_request() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7b; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };

    assert_eq!(
        nested.execute(request.clone()),
        request.clone().pending(),
        "a pending cancellation must not create a local terminal completion",
    );
    assert!(nested.release_failed());
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert!(executor.abandoned.is_empty());
    assert_eq!(executor.pending, Some(request));
}
#[test]
fn nested_action_stream_values_retain_pending_request() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7d; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xc9; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, vec![]).unwrap();

    let mut execute_executor = RecordingActionExecutor::new(None).with_execute_stream_values();
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut execute_executor,
        pending_request: None,
    };
    assert_eq!(
        nested.execute(request.clone()),
        request
            .clone()
            .stream_values(vec![RuntimeValue::Boolean(true)]),
    );
    assert_eq!(
        nested.pending_request_identity(),
        Some((request.request_id(), request.key(), request.generation(),))
    );
    drop(nested);
    assert_eq!(execute_executor.pending, Some(request.clone()));

    let mut cancel_executor = RecordingActionExecutor::new(None).with_cancel_stream_values();
    cancel_executor.pending = Some(request.clone());
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut cancel_executor,
        pending_request: None,
    };
    assert_eq!(
        nested.cancel(request.clone()),
        request
            .clone()
            .stream_values(vec![RuntimeValue::Boolean(true)]),
    );
    assert_eq!(
        nested.pending_request_identity(),
        Some((request.request_id(), request.key(), request.generation(),))
    );
    drop(nested);
    assert_eq!(cancel_executor.pending, Some(request));
}

#[test]
fn nested_action_stream_values_then_terminal_releases_child() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7e; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xca; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, vec![]).unwrap();
    let mut wrong_resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let wrong_request = wrong_resource
        .begin_stream_request(&active, vec![])
        .unwrap();
    assert_ne!(request.request_id(), wrong_request.request_id());

    let mut executor = StreamThenTerminalExecutor {
        calls: 0,
        stale: Some(wrong_request),
    };
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };

    assert_eq!(
        nested.execute(request.clone()),
        request
            .clone()
            .stream_values(vec![RuntimeValue::Boolean(true)]),
    );
    assert!(nested.release_failed());

    assert_eq!(
        nested.execute(request.clone()),
        request.clone().stream_completed(),
    );
    assert!(!nested.release_failed());

    assert_eq!(nested.execute(request.clone()), request.clone().pending());
    assert_eq!(
        nested.pending_request_identity(),
        Some((request.request_id(), request.key(), request.generation())),
    );
}

#[test]
fn nested_executor_rejects_mismatched_completion_identity() {
    let (active, function, pair, _) = version_one_active(true);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7d; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut wrong_resource =
        ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let wrong_request = wrong_resource.begin_request(&active, vec![]).unwrap();
    assert_ne!(request.request_id(), wrong_request.request_id());

    let identity = (request.request_id(), request.key(), request.generation());
    let mut execute_executor =
        RecordingActionExecutor::new(None).with_pending_identity(wrong_request.clone());
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut execute_executor,
        pending_request: None,
    };
    assert_eq!(nested.execute(request.clone()), request.clone().pending());
    assert_eq!(nested.pending_request_identity(), Some(identity));
    drop(nested);
    assert_eq!(
        execute_executor.cancelled,
        Vec::<ClientResourceRequest>::new()
    );
    assert_eq!(execute_executor.pending, Some(request.clone()));

    let mut cancel_executor =
        RecordingActionExecutor::new(None).with_cancel_pending_identity(wrong_request);
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut cancel_executor,
        pending_request: Some(request.clone()),
    };
    assert_eq!(nested.cancel(request.clone()), request.clone().pending());
    assert_eq!(nested.pending_request_identity(), Some(identity));
    drop(nested);
    assert_eq!(cancel_executor.cancelled, vec![request]);
}

#[test]
fn nested_abandon_mismatch_preserves_inner_request_without_local_marker() {
    let (active, function, pair, _) = version_one_active(true);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7c; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut state_a = ClientStateStore::new();
    let request_a = state_a
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut state_b = ClientStateStore::new();
    let request_b = state_b
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    assert_ne!(request_a.request_id(), request_b.request_id());

    let mut executor = RecordingActionExecutor::new(None);
    executor.pending = Some(request_a.clone());
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };

    assert_eq!(
        nested.abandon(request_b.clone()),
        Err("resource executor request mismatch".to_owned()),
    );
    assert_eq!(nested.pending_request_identity(), None);

    nested
        .abandon(request_a.clone())
        .expect("the retained child request remains addressable");
    drop(nested);
    assert_eq!(executor.pending, None);
    assert_eq!(executor.abandoned, vec![request_b, request_a]);
}

#[test]
fn nested_action_pending_cancel_retains_replacements_and_exact_child() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xfa; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe5; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let resource_parameter = ParameterId::from_bytes([0xd3; 16]);
    let nested_argument = FunctionArgument::new(
        resource_parameter,
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let nested_digest = ClientResourceKey::canonical_arguments_digest(
        &active,
        std::slice::from_ref(&nested_argument),
    )
    .unwrap();
    let resource_target = InvocationTarget::new(FunctionId::from_bytes([0xd1; 16]), pair);
    let replacement_a = ClientResourceKey::new(
        resource_target,
        auth.session_principal(),
        nested_digest,
        Sha256Digest::from_bytes([0xa1; 32]),
    );
    let replacement_b = ClientResourceKey::new(
        resource_target,
        auth.session_principal(),
        nested_digest,
        Sha256Digest::from_bytes([0xa2; 32]),
    );
    for replacement in [replacement_a, replacement_b] {
        state
            .get_or_create_resource(
                replacement,
                ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
            )
            .begin_loading()
            .unwrap();
    }
    assert_eq!(
        state.resource(replacement_a).map(ClientResource::status),
        Some(ClientResourceStatus::Loading),
    );
    assert_eq!(
        state.resource(replacement_b).map(ClientResource::status),
        Some(ClientResourceStatus::Loading),
    );
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();

    let error = trigger_client_action(
        &active,
        &action,
        &auth,
        &parent,
        &mut action_state,
        &[],
        &grants,
        &mut state,
        &mut executor,
    )
    .expect_err("pending child cancellation must retain the child request");
    let (request_id, child_key, generation) = match error {
        ClientActionError::ExecutorPending {
            request_id,
            key,
            generation,
            ..
        } => (request_id, key, generation),
        other => panic!("unexpected nested release error: {other:?}"),
    };
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(action_state.invocation_id(), None);
    assert_eq!(action_state.generation(), None);
    assert_eq!(executor.executed.len(), 1);
    assert_eq!(executor.cancelled, executor.executed);
    assert!(executor.abandoned.is_empty());
    let pending = executor
        .pending
        .clone()
        .expect("the executor retains the exact child request");
    assert_eq!(pending.request_id(), request_id);
    assert_eq!(pending.key(), child_key);
    assert_eq!(pending.key().target(), resource_target);
    assert_eq!(pending.generation(), generation);
    assert_ne!(child_key, replacement_a);
    assert_ne!(child_key, replacement_b);
    assert_eq!(
        state
            .resource(replacement_a)
            .expect("first replacement remains cached")
            .status(),
        ClientResourceStatus::Idle,
    );
    assert_eq!(
        state
            .resource(replacement_b)
            .expect("second replacement remains cached")
            .status(),
        ClientResourceStatus::Idle,
    );
    assert_eq!(
        state
            .resource(child_key)
            .expect("pending child remains cached")
            .status(),
        ClientResourceStatus::Loading,
    );
}

#[test]
fn nested_action_pending_poll_applies_exact_child_and_rejects_wrong_identity() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xfc; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe7; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let mut action_state = ClientActionState::default();
    let mut executor = PollingTestExecutor::default();

    let error = trigger_client_action(
        &active,
        &action,
        &auth,
        &parent,
        &mut action_state,
        &[],
        &grants,
        &mut state,
        &mut executor,
    )
    .expect_err("pending nested child must be handed back to the caller");
    let (request_id, child_key, generation) = match error {
        ClientActionError::ExecutorPending {
            request_id,
            key,
            generation,
            ..
        } => (request_id, key, generation),
        other => panic!("unexpected nested pending error: {other:?}"),
    };
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(action_state.invocation_id(), None);
    assert_eq!(action_state.generation(), None);
    let pending = executor
        .pending
        .clone()
        .expect("the executor retains the exact child request");
    assert_eq!(pending.request_id(), request_id);
    assert_eq!(pending.key(), child_key);
    assert_eq!(pending.generation(), generation);
    assert_eq!(
        state
            .resource(child_key)
            .expect("pending child remains cached")
            .status(),
        ClientResourceStatus::Loading,
    );

    let completion = executor.poll().expect("the retained child can be polled");
    assert_eq!(
        completion,
        ClientResourceCompletion::Ready {
            request_id,
            key: child_key,
            generation,
            value: RuntimeValue::Text("polled".to_owned()),
        }
    );
    state
        .resource_mut(child_key)
        .expect("retained child remains addressable")
        .apply_completion(&active, completion)
        .expect("the exact child completion publishes Ready");
    assert_eq!(
        state
            .resource(child_key)
            .expect("completed child remains cached")
            .status(),
        ClientResourceStatus::Ready,
    );
    assert_eq!(
        state
            .resource(child_key)
            .expect("completed child remains cached")
            .value(),
        Some(&RuntimeValue::Text("polled".to_owned())),
    );

    let before_wrong_completion = state
        .resource(child_key)
        .expect("completed child remains cached")
        .clone();
    let wrong_request_id = InvocationId::from_bytes([0xff; 16]);
    assert_eq!(
        state
            .resource_mut(child_key)
            .expect("completed child remains mutable")
            .apply_completion(
                &active,
                ClientResourceCompletion::Ready {
                    request_id: wrong_request_id,
                    key: child_key,
                    generation,
                    value: RuntimeValue::Boolean(false),
                },
            ),
        Err(super::super::ClientResourceError::RequestIdMismatch {
            expected: request_id,
            actual: wrong_request_id,
        }),
    );
    assert_eq!(
        state
            .resource(child_key)
            .expect("completed child remains cached")
            .clone(),
        before_wrong_completion,
    );
}

#[test]
fn nested_action_malformed_child_pending_cancel_retains_exact_identity() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xfb; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe6; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let mut action_state = ClientActionState::default();
    let mut executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Integer(7))).with_cancel_pending();

    let error = trigger_client_action(
        &active,
        &action,
        &auth,
        &parent,
        &mut action_state,
        &[],
        &grants,
        &mut state,
        &mut executor,
    )
    .expect_err("malformed child completion must remain pending");
    let (request_id, child_key, generation) = match error {
        ClientActionError::ExecutorPending {
            request_id,
            key,
            generation,
            ..
        } => (request_id, key, generation),
        other => panic!("unexpected malformed child error: {other:?}"),
    };

    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(executor.executed.len(), 1);
    assert_eq!(executor.cancelled, executor.executed);
    assert!(executor.abandoned.is_empty());
    let request = executor
        .executed
        .first()
        .expect("child request was submitted");
    assert_eq!(request.request_id(), request_id);
    assert_eq!(request.key(), child_key);
    assert_eq!(request.generation(), generation);
    assert_eq!(
        state
            .resource(child_key)
            .expect("malformed child remains cached")
            .status(),
        ClientResourceStatus::Loading,
    );
}

#[test]
fn action_local_resource_pending_is_cancelled_and_reports_cancelled_with_fresh_parent() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let enclosing_parent = InvocationId::from_bytes([0xf5; 16]);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: enclosing_parent,
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe2; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let mut executor = RecordingActionExecutor::new(None);

    for previous_parent in [None, Some(enclosing_parent)] {
        assert_eq!(
            trigger_client_action(
                &active,
                &action,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &grants,
                &mut state,
                &mut executor,
            ),
            Ok(ClientActionOutcome::Cancelled),
        );
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
        assert_eq!(executor.cancelled.len(), executor.executed.len());
        assert!(executor.abandoned.is_empty());
        let request = executor.executed.last().unwrap().clone();
        let cancelled = executor.cancelled.last().unwrap().clone();
        assert_eq!(request, cancelled);
        assert!(executor.poll().is_none());
        assert!(executor.pending.is_none());
        let nested_parent = request
            .invocation_context()
            .expect("nested resource carries invocation provenance")
            .parent_invocation_id();
        assert_ne!(nested_parent, enclosing_parent);
        if let Some(previous_parent) = previous_parent {
            assert_ne!(nested_parent, previous_parent);
        }
        assert!(state.resource(request.key()).is_none());
    }
}

#[test]
fn nested_action_with_loading_resource_reports_cancelled_without_dispatch() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf8; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe3; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let resource_parameter = ParameterId::from_bytes([0xd3; 16]);
    let nested_argument = FunctionArgument::new(
        resource_parameter,
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let nested_digest = ClientResourceKey::canonical_arguments_digest(
        &active,
        std::slice::from_ref(&nested_argument),
    )
    .unwrap();
    let nested_key = ClientResourceKey::new(
        InvocationTarget::new(FunctionId::from_bytes([0xd1; 16]), pair),
        auth.session_principal(),
        nested_digest,
        super::super::resource_invalidation_identity(
            active.catalogue_hash(),
            state.context().data_invalidation_token(),
            super::super::security_context_digest(&auth),
            state.context(),
            state.user_state_epoch(),
        ),
    );
    state
        .get_or_create_resource(
            nested_key,
            ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
        )
        .begin_request(&active, vec![nested_argument])
        .unwrap();
    let mut action_state = ClientActionState::default();
    let mut executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("unexpected".to_owned())));

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::from_grants([
                capability::LocalCapabilityGrant::new(
                    capability::LocalCapabilityName::StdFsRead,
                    capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
                )
                .unwrap(),
            ])
            .unwrap(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Cancelled),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert!(executor.executed.is_empty());
    assert_eq!(
        state
            .resource(nested_key)
            .expect("pre-existing nested resource remains cached")
            .status(),
        ClientResourceStatus::Loading,
    );
}

#[test]
fn nested_action_pending_cancel_clears_outer_action_state() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf9; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe4; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();

    let error = trigger_client_action(
        &active,
        &action,
        &auth,
        &parent,
        &mut action_state,
        &[],
        &grants,
        &mut state,
        &mut executor,
    )
    .expect_err("nested pending cancellation must be reported");
    let (request_id, key, generation) = match error {
        ClientActionError::ExecutorPending {
            request_id,
            key,
            generation,
            ..
        } => (request_id, key, generation),
        other => panic!("unexpected nested release error: {other:?}"),
    };
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(action_state.invocation_id(), None);
    assert_eq!(action_state.generation(), None);
    assert_eq!(executor.executed.len(), 1);
    assert_eq!(executor.cancelled, executor.executed);
    assert!(executor.abandoned.is_empty());
    let pending = executor
        .pending
        .clone()
        .expect("failed release retains child request");
    assert_eq!(pending.request_id(), request_id);
    assert_eq!(pending.key(), key);
    assert_eq!(pending.generation(), generation);
    assert_eq!(
        state
            .resource(key)
            .expect("failed release retains child resource")
            .status(),
        ClientResourceStatus::Loading,
    );

    executor
        .abandon(pending)
        .expect("caller can release the retained child request");
    assert!(executor.poll().is_none());
    assert_eq!(executor.late_dropped, 1);
    state
        .resource_mut(key)
        .expect("retained child remains addressable")
        .cancel(generation)
        .expect("caller can terminalise the retained child");
    assert_eq!(
        state
            .resource(key)
            .expect("retained child remains cached")
            .status(),
        ClientResourceStatus::Cancelled,
    );
}

#[test]
fn action_trigger_executes_a_verified_standard_server_target() {
    let (active, parent_function, _pair, parent_revision) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::Function(orna_standard::STD_INVOKE_ECHO_FUNCTION_ID),
        DefinitionReferenceKind::FunctionCall,
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        )
        .encode()
        .unwrap(),
    );
    let pair = active.pair();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("standard action fixture has a pinned snapshot");
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        CallSiteId::from_bytes([0xf6; 16]),
        vec![argument.clone()],
        orna_standard::INTEGER_TYPE_ID,
    );
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0xf7; 16]),
        observer_lineage: None,
    };
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Integer(42)));

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed),
    );
    let request = executor.executed.first().expect("action was dispatched");
    assert_eq!(
        request.target(),
        InvocationTarget::verified_standard(
            orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
            pair,
            standard.revision(),
            orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        )
    );
    assert_eq!(request.arguments(), &[argument]);
    assert_eq!(
        request.expected_type(),
        ResolvedType::Scalar(StandardScalar::Integer)
    );
    assert_ne!(
        request
            .invocation_context()
            .expect("server action carries invocation provenance")
            .call_site_id(),
        CallSiteId::from_bytes([0xf6; 16]),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_trigger_executes_a_local_client_target() {
    let (active, parent_function, target, pair, revision) = version_two_local_action_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf3; 16]),
        observer_lineage: None,
    };
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xf4; 16]),
        Vec::new(),
        orna_standard::BOOLEAN_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let registry = super::super::registered_opaque_codecs(
        active
            .catalogue_hash_context()
            .standard()
            .expect("action test fixture has a standard snapshot"),
    )
    .unwrap();
    let action = OpaqueValue::new(
        &active,
        &registry,
        super::super::STD_ACTION_TYPE_ID,
        payload,
    )
    .unwrap();
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Boolean(false))
    });

    assert_eq!(
        trigger_client_action(
            &active,
            &RuntimeValue::Opaque(action),
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_trigger_does_not_forward_forged_call_site_metadata() {
    let (active, parent_function, pair, parent_revision, _parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let forged_call_site = CallSiteId::from_bytes([0x9a; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0x9b; 16]),
        observer_lineage: None,
    };
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        forged_call_site,
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None);

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::Pending),
    );
    let request = executor.executed.first().expect("action was dispatched");
    let context = request
        .invocation_context()
        .expect("server action carries invocation provenance");
    assert_ne!(context.call_site_id(), forged_call_site);
    assert_eq!(
        context.parent_invocation_id(),
        parent.parent_invocation_id()
    );
    assert_eq!(request.target().function(), target);
}

#[test]
fn action_trigger_rejects_unreferenced_target_provenance() {
    let (original_active, target, pair, revision) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let standard_v6 = orna_standard::verify_standard_library_v6_snapshot(
        orna_standard::retained_standard_library_v6_snapshot().unwrap(),
    )
    .unwrap();
    let context = orna_core::revision::CatalogueHashContext::version_two(standard_v6);
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        original_active.catalogue(),
        original_active.function_revisions(),
        original_active.expressions(),
        original_active.origins(),
        original_active.references(),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            original_active.source().clone(),
            original_active.catalogue().clone(),
            catalogue_hash,
            ActiveRevisionContent::new(
                original_active.expressions().to_vec(),
                original_active.function_revisions().to_vec(),
                original_active.origins().to_vec(),
                original_active.references().to_vec(),
            ),
        ),
        context,
    )
    .unwrap();
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x75; 16]),
        Vec::new(),
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let registry = super::super::registered_opaque_codecs(
        active
            .catalogue_hash_context()
            .standard()
            .expect("action test fixture has a standard snapshot"),
    )
    .unwrap();
    let action = OpaqueValue::new(
        &active,
        &registry,
        super::super::STD_ACTION_TYPE_ID,
        payload,
    )
    .unwrap();
    let auth = authorise(pair, target);
    let parent = ClientExecutionContext {
        pair,
        function: target,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf2; 16]),
        observer_lineage: None,
    };
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Integer(1))
    });

    assert_eq!(
        trigger_client_action(
            &active,
            &RuntimeValue::Opaque(action),
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::TargetMismatch),
    );
}

#[test]
fn action_payload_rejects_noncanonical_argument_order() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let first =
        FunctionArgument::new(ParameterId::from_bytes([2; 16]), RuntimeValue::Integer(1)).unwrap();
    let second =
        FunctionArgument::new(ParameterId::from_bytes([1; 16]), RuntimeValue::Integer(2)).unwrap();
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([3; 16]),
        vec![first, second],
        orna_standard::INTEGER_TYPE_ID,
    );
    assert!(encode_action_payload(&active, &descriptor).is_err());
}
#[test]
fn client_artifact_integrity_checks_domain_and_payload_digest() {
    let payload = b"client-artifact-demo".to_vec();
    let digest = artifact_payload_digest(&payload).expect("demo payload digest");
    let valid = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "demo.client-artifact",
        1,
        payload.clone(),
        digest,
    )
    .expect("valid client artifact");
    assert_eq!(
        super::super::validate_client_artifact_integrity(&valid),
        Ok(())
    );

    let wrong_kind = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "demo.client-artifact",
        1,
        payload.clone(),
        digest,
    )
    .expect("wrong-domain artifact");
    assert_eq!(
        super::super::validate_client_artifact_integrity(&wrong_kind),
        Err(super::super::ClientArtifactIntegrityError::WrongExecutionDomain)
    );

    let wrong_digest = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "demo.client-artifact",
        1,
        payload,
        Sha256Digest::from_bytes([0; 32]),
    )
    .expect("wrong-digest artifact");
    assert_eq!(
        super::super::validate_client_artifact_integrity(&wrong_digest),
        Err(super::super::ClientArtifactIntegrityError::PayloadDigest)
    );
}
