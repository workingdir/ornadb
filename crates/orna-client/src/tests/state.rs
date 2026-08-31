use super::*;

#[test]
fn evaluates_version_four_state_plans_and_initialises_local_and_session_state() {
    let (active, function, plan) = version_four_text_state_plan();
    let mut state = super::super::ClientStateStore::new();

    let result = super::super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("hello world".to_owned())
    );
    assert_eq!(
        state.local().get(&super::super::ClientStateKey::new(
            function,
            StateSlotId::from_bytes([0x11; 16])
        )),
        Some(&RuntimeValue::Text("local-default".to_owned()))
    );
    let expected_null = RuntimeValue::null(ResolvedType::value(
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    ))
    .unwrap();
    assert_eq!(
        state.session().get(&super::super::ClientStateKey::new(
            function,
            StateSlotId::from_bytes([0x12; 16])
        )),
        Some(&expected_null)
    );
    assert!(
        !state
            .local()
            .contains_key(&super::super::ClientStateKey::new(
                function,
                StateSlotId::from_bytes([0x13; 16])
            ))
    );
    assert!(state.user().is_empty());
    assert_eq!(
        plan.format_version(),
        orna_artifact::client_plan::STATE_FORMAT_VERSION
    );

    super::super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();
}

#[test]
fn state_context_data_invalidation_token_preserves_existing_defaults() {
    let function = FunctionId::from_bytes([0x61; 16]);
    let mut context = super::super::ClientStateContext::default_for(function);
    assert_eq!(
        context.data_invalidation_token(),
        Sha256Digest::from_bytes([0; 32])
    );
    assert_eq!(
        super::super::ClientStateContext::new(
            function,
            "profile".to_owned(),
            "instance".to_owned()
        )
        .unwrap()
        .data_invalidation_token(),
        Sha256Digest::from_bytes([0; 32]),
    );
    let token = Sha256Digest::from_bytes([0x62; 32]);
    context.set_data_invalidation_token(token);
    assert_eq!(context.data_invalidation_token(), token);
    assert_eq!(context.root_function(), function);
    assert_eq!(context.state_profile(), "");
    assert_eq!(context.instance_key(), "");
}

#[test]
fn version_four_state_context_profiles_are_isolated() {
    let (active, function, _) = version_four_text_state_plan();
    let profile_a =
        super::super::ClientStateContext::new(function, "profile-a".to_owned(), String::new())
            .unwrap();
    let profile_b =
        super::super::ClientStateContext::new(function, "profile-b".to_owned(), String::new())
            .unwrap();
    let mut state = super::super::ClientStateStore::new();
    let grants = super::super::capability::LocalCapabilityGrantSet::new();
    let slot = StateSlotId::from_bytes([0x12; 16]);

    super::super::evaluate_client_function_in_state_context_with_grants_and_arguments(
        &active,
        &authorise(active.pair(), function),
        &profile_a,
        &[],
        &[],
        &grants,
        &mut state,
    )
    .unwrap();
    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |_: &super::super::ClientResourceRequest| Ok(RuntimeValue::Boolean(true)),
    );
    super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(active.pair(), function),
        &profile_b,
        &[],
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0x44; 16]),
        &mut executor,
    )
    .unwrap();

    let key_a = super::super::ClientStateKey::from_context(&profile_a, function, slot);
    let key_b = super::super::ClientStateKey::from_context(&profile_b, function, slot);
    assert_ne!(key_a, key_b);
    assert!(state.session().contains_key(&key_a));
    assert!(state.session().contains_key(&key_b));
    assert_eq!(state.context(), &profile_b);
}

#[test]
fn version_four_keeps_caller_state_input_over_the_plan_default() {
    let (active, function, _) = version_four_text_state_plan();
    let mut state = super::super::ClientStateStore::new();
    state.session_mut().insert(
        super::super::ClientStateKey::new(function, StateSlotId::from_bytes([0x12; 16])),
        RuntimeValue::Text("remounted-session".to_owned()),
    );

    super::super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();

    assert_eq!(
        state.session().get(&super::super::ClientStateKey::new(
            function,
            StateSlotId::from_bytes([0x12; 16])
        )),
        Some(&RuntimeValue::Text("remounted-session".to_owned()))
    );
}

#[test]
fn version_four_rejects_caller_state_with_the_wrong_type() {
    let (active, function, _) = version_four_text_state_plan();
    let mut state = super::super::ClientStateStore::new();
    state.session_mut().insert(
        super::super::ClientStateKey::new(function, StateSlotId::from_bytes([0x12; 16])),
        RuntimeValue::Boolean(true),
    );

    let error = super::super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::super::ClientExecutionError::StateEvaluation {
            context,
            source: super::super::ClientStateError::StoredTypeMismatch { slot },

        } if context.function() == function
            && *slot == StateSlotId::from_bytes([0x12; 16])
    ));
}

#[test]
fn version_four_user_state_with_matching_persisted_type_is_accepted() {
    let slot = StateSlotId::from_bytes([0x20; 16]);
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![orna_artifact::client_plan::StateSlot::new(
            slot,
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::User,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::super::ClientStateStore::new();
    state.set_context(super::super::ClientStateContext::default_for(function));
    let durable_key = UserStateKey::new(
        PrincipalId::from_bytes([0x7a; 16]),
        function,
        String::new(),
        function,
        String::new(),
        slot,
    )
    .unwrap();
    state
        .load_user_state(&[UserStateCell::new(
            durable_key,
            RuntimeValue::Boolean(true),
            orna_standard::BOOLEAN_TYPE_ID,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();

    let result = super::super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();

    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    assert_eq!(state.user().len(), 1);
    assert_eq!(
        state
            .user()
            .values()
            .next()
            .expect("the matching USER state remains loaded")
            .value_type(),
        orna_standard::BOOLEAN_TYPE_ID,
    );
}

#[test]
fn version_four_user_state_rejects_wrong_persisted_type_without_mutating_state() {
    let slot = StateSlotId::from_bytes([0x22; 16]);
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![orna_artifact::client_plan::StateSlot::new(
            slot,
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::User,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::super::ClientStateStore::new();
    state.set_context(super::super::ClientStateContext::default_for(function));
    let durable_key = UserStateKey::new(
        PrincipalId::from_bytes([0x7a; 16]),
        function,
        String::new(),
        function,
        String::new(),
        slot,
    )
    .unwrap();
    state
        .load_user_state(&[UserStateCell::new(
            durable_key,
            RuntimeValue::Boolean(true),
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    let before = state.clone();

    let error = super::super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientExecutionError::StateEvaluation {
            context,
            source: super::super::ClientStateError::StoredTypeMismatch { slot: actual_slot },
        } if context.function() == function && actual_slot == slot
    ));
    assert_eq!(state, before);
}

#[test]
fn version_four_user_state_without_persisted_value_uses_unset_default() {
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![orna_artifact::client_plan::StateSlot::new(
            StateSlotId::from_bytes([0x21; 16]),
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::User,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::super::ClientStateStore::new();

    let result = super::super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();

    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    assert!(state.user().is_empty());
    assert!(state.local().is_empty() && state.session().is_empty());
}
#[test]
fn client_user_state_store_loads_updates_and_applies_write_results() {
    let root_function = FunctionId::from_bytes([0x31; 16]);
    let function = FunctionId::from_bytes([0x32; 16]);
    let slot = StateSlotId::from_bytes([0x33; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let client_key = super::super::ClientStateKey::from_context(&context, function, slot);
    let durable_key = UserStateKey::new(
        PrincipalId::from_bytes([0x34; 16]),
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let cell = UserStateCell::new(
        durable_key,
        RuntimeValue::Text("loaded".to_owned()),
        value_type,
        7,
        SystemTime::UNIX_EPOCH,
    );
    let mut state = super::super::ClientStateStore::new();
    state.set_context(context);
    state.load_user_state(&[cell]).unwrap();
    assert!(state.pending_user_state_changes().unwrap().is_empty());

    state
        .set_user_state(
            client_key.clone(),
            RuntimeValue::Text("changed".to_owned()),
            value_type,
        )
        .unwrap();
    let changes = state.pending_user_state_changes().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].expected_revision(), Some(7));
    let before = state.user().clone();
    let leap_result = UserStateWriteResult::new(
        changes[0].key_without_principal(),
        UserStateWriteOutcome::Written { revision: 9 },
    );
    let leap_error = state
        .apply_user_state_write_results(&changes, &[leap_result])
        .unwrap_err();
    assert!(matches!(
        leap_error,
        super::super::ClientUserStateError::InvalidRevision(key) if key == client_key
    ));
    assert_eq!(state.user(), &before);
    assert_eq!(state.pending_user_state_changes().unwrap(), changes);

    let result = UserStateWriteResult::new(
        changes[0].key_without_principal(),
        UserStateWriteOutcome::Written { revision: 8 },
    );
    state
        .apply_user_state_write_results(&changes, &[result])
        .unwrap();

    let stored = state.user().get(&client_key).unwrap();
    assert_eq!(stored.value(), &RuntimeValue::Text("changed".to_owned()));
    assert_eq!(stored.revision(), Some(8));
    assert!(!stored.is_dirty());
    assert!(state.pending_user_state_changes().unwrap().is_empty());
}

#[test]
fn client_user_state_set_rejects_context_mismatch_before_lookup_or_mutation() {
    let root_function = FunctionId::from_bytes([0xc1; 16]);
    let function = FunctionId::from_bytes([0xc2; 16]);
    let slot = StateSlotId::from_bytes([0xc3; 16]);
    let principal = PrincipalId::from_bytes([0xc4; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let other_context = super::super::ClientStateContext::new(
        FunctionId::from_bytes([0xc5; 16]),
        "other-profile".to_owned(),
        "other-instance".to_owned(),
    )
    .unwrap();
    let matching_key = super::super::ClientStateKey::from_context(&context, function, slot);
    let mismatched_key = super::super::ClientStateKey::from_context(&other_context, function, slot);
    let durable_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let mut state = super::super::ClientStateStore::new();
    state.set_context(context);
    state
        .load_user_state(&[UserStateCell::new(
            durable_key,
            RuntimeValue::Text("loaded".to_owned()),
            value_type,
            7,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    let before = state.user().clone();
    let pending_before = state.pending_user_state_changes().unwrap();

    let error = state
        .set_user_state(
            mismatched_key.clone(),
            RuntimeValue::Boolean(true),
            orna_standard::BOOLEAN_TYPE_ID,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientUserStateError::ContextMismatch(key) if key == mismatched_key
    ));
    assert_eq!(state.user(), &before);
    assert_eq!(state.pending_user_state_changes().unwrap(), pending_before);

    state
        .set_user_state(
            matching_key.clone(),
            RuntimeValue::Text("changed".to_owned()),
            value_type,
        )
        .unwrap();
    let stored = state.user().get(&matching_key).unwrap();
    assert_eq!(stored.value(), &RuntimeValue::Text("changed".to_owned()));
    assert_eq!(stored.revision(), Some(7));
    assert!(stored.is_dirty());
    let changes = state.pending_user_state_changes().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].expected_revision(), Some(7));
}

#[test]
fn client_user_state_load_rejects_mixed_context_batch_atomically() {
    let root_function = FunctionId::from_bytes([0x71; 16]);
    let function = FunctionId::from_bytes([0x72; 16]);
    let slot = StateSlotId::from_bytes([0x73; 16]);
    let principal = PrincipalId::from_bytes([0x74; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let matching_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let mismatched_key = UserStateKey::new(
        principal,
        FunctionId::from_bytes([0x75; 16]),
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let mut state = super::super::ClientStateStore::new();
    state.set_context(context);
    state
        .load_user_state(&[UserStateCell::new(
            matching_key.clone(),
            RuntimeValue::Text("before".to_owned()),
            value_type,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    let before = state.user().clone();
    let before_epoch = state.user_state_epoch();

    let error = state
        .load_user_state(&[
            UserStateCell::new(
                matching_key,
                RuntimeValue::Text("replacement".to_owned()),
                value_type,
                2,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                mismatched_key,
                RuntimeValue::Text("outside-context".to_owned()),
                value_type,
                3,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientUserStateError::ContextMismatch(key)
            if key.root_function() == FunctionId::from_bytes([0x75; 16])
    ));
    assert_eq!(state.user(), &before);
    assert_eq!(state.user_state_epoch(), before_epoch);
}

#[test]
fn client_user_state_reload_advances_epoch_for_changed_state_and_skips_identical_snapshot() {
    let root_function = FunctionId::from_bytes([0x76; 16]);
    let function = FunctionId::from_bytes([0x77; 16]);
    let slot = StateSlotId::from_bytes([0x78; 16]);
    let principal = PrincipalId::from_bytes([0x79; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let loaded = UserStateCell::new(
        key.clone(),
        RuntimeValue::Text("loaded".to_owned()),
        value_type,
        1,
        SystemTime::UNIX_EPOCH,
    );
    let mut state = super::super::ClientStateStore::new();
    state.set_context(context);
    assert_eq!(state.user_state_epoch(), 0);

    state
        .load_user_state(std::slice::from_ref(&loaded))
        .unwrap();
    let first_epoch = state.user_state_epoch();
    assert_eq!(first_epoch, 1);

    state
        .load_user_state(std::slice::from_ref(&loaded))
        .unwrap();
    assert_eq!(
        state.user_state_epoch(),
        first_epoch,
        "an identical durable snapshot does not invalidate resources"
    );

    let changed = UserStateCell::new(
        key,
        RuntimeValue::Text("changed".to_owned()),
        value_type,
        2,
        SystemTime::UNIX_EPOCH,
    );
    state
        .load_user_state(std::slice::from_ref(&changed))
        .unwrap();
    assert_eq!(state.user_state_epoch(), first_epoch + 1);

    state.load_user_state(&[]).unwrap();
    assert_eq!(state.user_state_epoch(), first_epoch + 2);
    assert!(state.user().is_empty());

    state.user_state_epoch = u64::MAX;
    let before_overflow = state.clone();
    let error = state
        .load_user_state(std::slice::from_ref(&loaded))
        .unwrap_err();
    assert_eq!(
        error,
        super::super::ClientUserStateError::InvalidChange(
            "USER state invalidation epoch exhausted".to_owned()
        )
    );
    assert_eq!(state, before_overflow);
}

#[test]
fn client_user_state_load_accepts_multiple_instances_and_rejects_foreign_context_atomically() {
    let root_function = FunctionId::from_bytes([0x81; 16]);
    let function = FunctionId::from_bytes([0x82; 16]);
    let slot = StateSlotId::from_bytes([0x83; 16]);
    let principal = PrincipalId::from_bytes([0x84; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "active-instance".to_owned(),
    )
    .unwrap();
    let active_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "active-instance".to_owned(),
        slot,
    )
    .unwrap();
    let dynamic_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "row:42".to_owned(),
        slot,
    )
    .unwrap();
    let selected_absent_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "row:empty".to_owned(),
        slot,
    )
    .unwrap();
    let unselected_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "row:99".to_owned(),
        slot,
    )
    .unwrap();
    let foreign_key = UserStateKey::new(
        principal,
        FunctionId::from_bytes([0x85; 16]),
        "foreign-profile".to_owned(),
        function,
        "foreign-instance".to_owned(),
        slot,
    )
    .unwrap();
    let requested_instances = vec![
        (function, "active-instance".to_owned()),
        (function, "row:42".to_owned()),
        (function, "row:empty".to_owned()),
    ];
    let unselected_client_key = super::super::ClientStateKey::from_user_cell(&UserStateCell::new(
        unselected_key.clone(),
        RuntimeValue::Text("unselected-before".to_owned()),
        value_type,
        1,
        SystemTime::UNIX_EPOCH,
    ));
    let dynamic_client_key = super::super::ClientStateKey::from_user_cell(&UserStateCell::new(
        dynamic_key.clone(),
        RuntimeValue::Text("dynamic-loaded".to_owned()),
        value_type,
        3,
        SystemTime::UNIX_EPOCH,
    ));
    let mut state = super::super::ClientStateStore::new();
    state.set_context(context.clone());

    state
        .load_user_state(&[
            UserStateCell::new(
                active_key.clone(),
                RuntimeValue::Text("active-before".to_owned()),
                value_type,
                1,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                unselected_key.clone(),
                RuntimeValue::Text("unselected-before".to_owned()),
                value_type,
                1,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                selected_absent_key.clone(),
                RuntimeValue::Text("selected-before".to_owned()),
                value_type,
                1,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap();
    state.set_context(
        super::super::ClientStateContext::new(
            root_function,
            "profile".to_owned(),
            "row:99".to_owned(),
        )
        .unwrap(),
    );
    state
        .set_user_state(
            unselected_client_key.clone(),
            RuntimeValue::Text("unselected-dirty".to_owned()),
            value_type,
        )
        .unwrap();
    state.set_context(context.clone());
    let epoch_before_filtered = state.user_state_epoch();

    state
        .load_user_state_for_instances(
            &[
                UserStateCell::new(
                    active_key.clone(),
                    RuntimeValue::Text("active-loaded".to_owned()),
                    value_type,
                    2,
                    SystemTime::UNIX_EPOCH,
                ),
                UserStateCell::new(
                    dynamic_key.clone(),
                    RuntimeValue::Text("dynamic-loaded".to_owned()),
                    value_type,
                    3,
                    SystemTime::UNIX_EPOCH,
                ),
            ],
            &requested_instances,
        )
        .unwrap();
    assert_eq!(
        state.user_state_epoch(),
        epoch_before_filtered + 1,
        "filtered reload changes the selected USER snapshot"
    );

    assert_eq!(state.user().len(), 3);
    assert!(
        !state
            .user()
            .contains_key(&super::super::ClientStateKey::from_user_cell(
                &UserStateCell::new(
                    selected_absent_key.clone(),
                    RuntimeValue::Text("selected-before".to_owned()),
                    value_type,
                    1,
                    SystemTime::UNIX_EPOCH,
                ),
            ))
    );
    assert_eq!(
        state
            .user()
            .get(&super::super::ClientStateKey::from_user_cell(
                &UserStateCell::new(
                    active_key.clone(),
                    RuntimeValue::Text("active-loaded".to_owned()),
                    value_type,
                    2,
                    SystemTime::UNIX_EPOCH,
                ),
            ))
            .map(super::super::ClientUserState::value),
        Some(&RuntimeValue::Text("active-loaded".to_owned())),
    );
    assert_eq!(
        state
            .user()
            .get(&super::super::ClientStateKey::from_user_cell(
                &UserStateCell::new(
                    dynamic_key.clone(),
                    RuntimeValue::Text("dynamic-loaded".to_owned()),
                    value_type,
                    3,
                    SystemTime::UNIX_EPOCH,
                ),
            ))
            .map(super::super::ClientUserState::value),
        Some(&RuntimeValue::Text("dynamic-loaded".to_owned())),
    );

    let unselected = state.user().get(&unselected_client_key).unwrap();
    assert_eq!(
        unselected.value(),
        &RuntimeValue::Text("unselected-dirty".to_owned()),
    );
    assert!(unselected.is_dirty());
    let pending = state.pending_user_state_changes().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].instance_key(), "row:99");

    let set_error = state
        .set_user_state(
            dynamic_client_key.clone(),
            RuntimeValue::Text("must-not-set-dynamic".to_owned()),
            value_type,
        )
        .unwrap_err();
    assert!(matches!(
        set_error,
        super::super::ClientUserStateError::ContextMismatch(key) if key == dynamic_client_key
    ));

    let before_unexpected = state.user().clone();
    let epoch_before_unexpected = state.user_state_epoch();

    let unexpected_error = state
        .load_user_state_for_instances(
            &[UserStateCell::new(
                unselected_key.clone(),
                RuntimeValue::Text("unexpected-instance".to_owned()),
                value_type,
                4,
                SystemTime::UNIX_EPOCH,
            )],
            &requested_instances,
        )
        .unwrap_err();
    assert!(matches!(
        unexpected_error,
        super::super::ClientUserStateError::ContextMismatch(key)
            if key.instance_key() == "row:99"
    ));
    assert_eq!(state.user(), &before_unexpected);
    assert_eq!(state.user_state_epoch(), epoch_before_unexpected);

    let before_duplicate = state.user().clone();
    let epoch_before_duplicate = state.user_state_epoch();

    let duplicate_error = state
        .load_user_state(&[
            UserStateCell::new(
                dynamic_key.clone(),
                RuntimeValue::Text("duplicate-first".to_owned()),
                value_type,
                4,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                dynamic_key.clone(),
                RuntimeValue::Text("duplicate-second".to_owned()),
                value_type,
                5,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap_err();
    assert!(matches!(
        duplicate_error,
        super::super::ClientUserStateError::DuplicateKey(key)
            if key.instance_key() == "row:42"
    ));
    assert_eq!(state.user(), &before_duplicate);
    assert_eq!(state.user_state_epoch(), epoch_before_duplicate);

    let before_foreign = state.user().clone();
    let error = state
        .load_user_state(&[
            UserStateCell::new(
                active_key,
                RuntimeValue::Text("must-not-replace".to_owned()),
                value_type,
                4,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                foreign_key,
                RuntimeValue::Text("must-not-load".to_owned()),
                value_type,
                5,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap_err();
    assert!(matches!(
        error,
        super::super::ClientUserStateError::ContextMismatch(key)
            if key.root_function() == FunctionId::from_bytes([0x85; 16])
                && key.state_profile() == "foreign-profile"
    ));
    assert_eq!(state.user(), &before_foreign);
}

#[test]
fn client_user_state_empty_filter_accepts_the_default_instance_cell() {
    let root_function = FunctionId::from_bytes([0xa6; 16]);
    let function = FunctionId::from_bytes([0xa7; 16]);
    let slot = StateSlotId::from_bytes([0xa8; 16]);
    let principal = PrincipalId::from_bytes([0xa9; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "mounted-instance".to_owned(),
    )
    .unwrap();
    let default_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        String::new(),
        slot,
    )
    .unwrap();
    let default_cell = UserStateCell::new(
        default_key,
        RuntimeValue::Text("default".to_owned()),
        value_type,
        1,
        SystemTime::UNIX_EPOCH,
    );
    let client_key = super::super::ClientStateKey::from_user_cell(&default_cell);
    let mut state = super::super::ClientStateStore::new();
    state.set_context(context);

    state
        .load_user_state_for_instances(&[default_cell], &[])
        .unwrap();

    assert_eq!(
        state
            .user()
            .get(&client_key)
            .map(super::super::ClientUserState::value),
        Some(&RuntimeValue::Text("default".to_owned())),
    );
    assert_eq!(
        state
            .user()
            .get(&client_key)
            .map(super::super::ClientUserState::revision),
        Some(Some(1)),
    );
}

#[test]
fn client_user_state_load_replaces_prior_context_cells() {
    let root_function = FunctionId::from_bytes([0x61; 16]);
    let function = FunctionId::from_bytes([0x62; 16]);
    let slot = StateSlotId::from_bytes([0x63; 16]);
    let other_root_function = FunctionId::from_bytes([0x64; 16]);
    let other_function = FunctionId::from_bytes([0x65; 16]);
    let other_slot = StateSlotId::from_bytes([0x66; 16]);
    let principal = PrincipalId::from_bytes([0x67; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let other_context = super::super::ClientStateContext::new(
        other_root_function,
        "other-profile".to_owned(),
        "other-instance".to_owned(),
    )
    .unwrap();
    let current_client_key = super::super::ClientStateKey::from_context(&context, function, slot);
    let other_client_key =
        super::super::ClientStateKey::from_context(&other_context, other_function, other_slot);
    let current_durable_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let other_durable_key = UserStateKey::new(
        principal,
        other_root_function,
        "other-profile".to_owned(),
        other_function,
        "other-instance".to_owned(),
        other_slot,
    )
    .unwrap();
    let mut state = super::super::ClientStateStore::new();

    state.set_context(context.clone());
    state
        .load_user_state(&[UserStateCell::new(
            current_durable_key,
            RuntimeValue::Text("principal-a".to_owned()),
            value_type,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    state.set_context(other_context);
    state
        .load_user_state(&[UserStateCell::new(
            other_durable_key,
            RuntimeValue::Text("other-context".to_owned()),
            value_type,
            2,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();

    state.set_context(context);
    state.load_user_state(&[]).unwrap();

    assert!(!state.user().contains_key(&current_client_key));
    assert!(state.user().contains_key(&other_client_key));
}

#[test]
fn client_user_state_binding_rejects_other_session_without_mutating_cells_or_pending_changes() {
    let root_function = FunctionId::from_bytes([0x91; 16]);
    let function = FunctionId::from_bytes([0x92; 16]);
    let first_slot = StateSlotId::from_bytes([0x93; 16]);
    let second_slot = StateSlotId::from_bytes([0x94; 16]);
    let principal = PrincipalId::from_bytes([0x95; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x96; 16]),
        CatalogueRevisionId::from_bytes([0x97; 16]),
    );
    let snapshot = SecuritySnapshot::new(
        pair,
        vec![],
        vec![Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    )
    .expect("session binding fixture should be valid");
    let first_session = snapshot
        .bind_authenticated_session(principal, vec![])
        .expect("first session should bind");
    let second_session = snapshot
        .bind_authenticated_session(principal, vec![])
        .expect("second session should bind");
    assert_eq!(first_session.binding(), first_session.clone().binding());
    assert_ne!(first_session.binding(), second_session.binding());

    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let first_client_key =
        super::super::ClientStateKey::from_context(&context, function, first_slot);
    let second_client_key =
        super::super::ClientStateKey::from_context(&context, function, second_slot);
    let first_durable_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        first_slot,
    )
    .unwrap();
    let second_durable_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        second_slot,
    )
    .unwrap();
    let mut state = super::super::ClientStateStore::new();
    state.set_context(context);
    assert!(
        state
            .bind_authenticated_session(first_session.binding())
            .is_ok()
    );
    assert!(
        state
            .bind_authenticated_session(first_session.clone().binding())
            .is_ok()
    );
    state
        .load_user_state(&[
            UserStateCell::new(
                first_durable_key,
                RuntimeValue::Text("first".to_owned()),
                value_type,
                4,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                second_durable_key,
                RuntimeValue::Text("second".to_owned()),
                value_type,
                8,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap();
    state
        .set_user_state(
            first_client_key,
            RuntimeValue::Text("first-dirty".to_owned()),
            value_type,
        )
        .unwrap();
    let durable_before = state.user().clone();
    let pending_before = state.pending_user_state_changes().unwrap();
    assert_eq!(pending_before.len(), 1);

    assert_eq!(
        state.bind_authenticated_session(second_session.binding()),
        Err(super::super::ClientUserStateError::SessionMismatch)
    );
    assert_eq!(state.user(), &durable_before);
    assert_eq!(state.pending_user_state_changes().unwrap(), pending_before);
    assert!(
        state
            .bind_authenticated_session(first_session.binding())
            .is_ok()
    );
    assert_eq!(state.user().len(), 2);
    assert!(state.user().contains_key(&second_client_key));
}

#[test]
fn client_user_state_store_rejects_first_write_revision_leap() {
    let root_function = FunctionId::from_bytes([0x51; 16]);
    let function = FunctionId::from_bytes([0x52; 16]);
    let slot = StateSlotId::from_bytes([0x53; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let client_key = super::super::ClientStateKey::from_context(&context, function, slot);
    let mut state = super::super::ClientStateStore::new();
    state.set_context(context);
    state
        .set_user_state(
            client_key.clone(),
            RuntimeValue::Text("new".to_owned()),
            value_type,
        )
        .unwrap();
    let changes = state.pending_user_state_changes().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].expected_revision(), None);
    let before = state.user().clone();
    let result = UserStateWriteResult::new(
        changes[0].key_without_principal(),
        UserStateWriteOutcome::Written { revision: 2 },
    );

    let error = state
        .apply_user_state_write_results(&changes, &[result])
        .unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientUserStateError::InvalidRevision(key) if key == client_key
    ));
    assert_eq!(state.user(), &before);
    assert_eq!(state.pending_user_state_changes().unwrap(), changes);
}

#[test]
fn client_user_state_write_results_are_atomic_for_invalid_revision_or_conflict() {
    let root_function = FunctionId::from_bytes([0x41; 16]);
    let function = FunctionId::from_bytes([0x42; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let first_slot = StateSlotId::from_bytes([0x43; 16]);
    let second_slot = StateSlotId::from_bytes([0x44; 16]);
    let principal = PrincipalId::from_bytes([0x45; 16]);
    let first_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        first_slot,
    )
    .unwrap();
    let second_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        second_slot,
    )
    .unwrap();
    let first_client_key =
        super::super::ClientStateKey::from_context(&context, function, first_slot);
    let second_client_key =
        super::super::ClientStateKey::from_context(&context, function, second_slot);
    let mut state = super::super::ClientStateStore::new();
    state.set_context(context);
    state
        .load_user_state(&[
            UserStateCell::new(
                first_key,
                RuntimeValue::Text("first-loaded".to_owned()),
                value_type,
                7,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                second_key,
                RuntimeValue::Text("second-loaded".to_owned()),
                value_type,
                11,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap();
    state
        .set_user_state(
            first_client_key.clone(),
            RuntimeValue::Text("first-changed".to_owned()),
            value_type,
        )
        .unwrap();
    state
        .set_user_state(
            second_client_key.clone(),
            RuntimeValue::Text("second-changed".to_owned()),
            value_type,
        )
        .unwrap();
    let changes = state.pending_user_state_changes().unwrap();
    assert_eq!(changes.len(), 2);
    let before = state.user().clone();
    let results = vec![
        UserStateWriteResult::new(
            changes[0].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 8 },
        ),
        UserStateWriteResult::new(
            changes[1].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 0 },
        ),
    ];

    let error = state
        .apply_user_state_write_results(&changes, &results)
        .unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientUserStateError::InvalidRevision(_)
    ));
    assert_eq!(state.user(), &before);

    let mixed_results = vec![
        UserStateWriteResult::new(
            changes[0].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 8 },
        ),
        UserStateWriteResult::new(
            changes[1].key_without_principal(),
            UserStateWriteOutcome::Conflict {
                current_revision: 15,
            },
        ),
    ];
    let mixed_error = state
        .apply_user_state_write_results(&changes, &mixed_results)
        .unwrap_err();

    assert!(matches!(
        mixed_error,
        super::super::ClientUserStateError::Conflict {
            key,
            expected: Some(11),
            current: 15,
        } if key == second_client_key
    ));
    assert_eq!(state.user(), &before);

    let duplicate_changes = vec![changes[1].clone(), changes[1].clone()];
    let duplicate_results = vec![
        UserStateWriteResult::new(
            duplicate_changes[0].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 16 },
        ),
        UserStateWriteResult::new(
            duplicate_changes[1].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 17 },
        ),
    ];
    let duplicate_error = state
        .apply_user_state_write_results(&duplicate_changes, &duplicate_results)
        .unwrap_err();

    assert!(matches!(
        duplicate_error,
        super::super::ClientUserStateError::DuplicateKey(key) if key == super::super::ClientStateKey::from_user_change(&changes[1])
    ));
    assert_eq!(state.user(), &before);
}

#[test]
fn version_four_state_default_type_mismatch_fails_closed() {
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![
            orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x30; 16]),
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::String {
                        value: "must-not-commit".to_owned(),
                    },
                ),
            ),
            orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x31; 16]),
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::String {
                        value: "not-a-boolean".to_owned(),
                    },
                ),
            ),
        ],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::super::ClientStateStore::new();

    let error = super::super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap_err();
    assert!(state.local().is_empty());

    assert!(matches!(
        &error,
        super::super::ClientExecutionError::StateEvaluation {
            context,
            source: super::super::ClientStateError::DefaultTypeMismatch { slot },
        } if context.function() == function
            && *slot == StateSlotId::from_bytes([0x31; 16])
    ));
}

#[test]
fn version_four_state_initializer_stages_all_scopes_before_commit() {
    let local_slot = StateSlotId::from_bytes([0x30; 16]);
    let session_slot = StateSlotId::from_bytes([0x31; 16]);
    let user_slot = StateSlotId::from_bytes([0x32; 16]);
    let invalid_slot = StateSlotId::from_bytes([0x33; 16]);
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![
            orna_artifact::client_plan::StateSlot::new(
                local_slot,
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::String {
                        value: "must-not-commit-local".to_owned(),
                    },
                ),
            ),
            orna_artifact::client_plan::StateSlot::new(
                session_slot,
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::StateScope::Session,
                orna_artifact::client_plan::StateDefault::Null,
            ),
            orna_artifact::client_plan::StateSlot::new(
                user_slot,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::User,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
                ),
            ),
            orna_artifact::client_plan::StateSlot::new(
                invalid_slot,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::String {
                        value: "not-a-boolean".to_owned(),
                    },
                ),
            ),
        ],
    );
    let (active, function, pair, function_revision) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let execution_context = super::super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0x34; 16]),
        observer_lineage: None,
    };
    let state_context = super::super::ClientStateContext::new(
        function,
        "atomic-profile".to_owned(),
        "atomic-instance".to_owned(),
    )
    .unwrap();
    let mut state = super::super::ClientStateStore::new();
    state.set_context(state_context);
    let before = state.clone();
    let mut executor: Option<&mut dyn super::super::ClientResourceExecutor> = None;
    let mut local_environment = super::super::ClientLocalEnvironment::new();
    let mut fuel = super::super::ClientExecutionFuel::new();

    let error = super::super::initialize_client_state(
        &active,
        &plan,
        execution_context,
        super::super::ObserverLineage::top_level(execution_context.parent_invocation_id()),
        &[],
        &[],
        &super::super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        PrincipalId::from_bytes([0x35; 16]),
        &mut executor,
        &mut local_environment,
        &mut fuel,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientExecutionError::StateEvaluation {
            context,
            source: super::super::ClientStateError::DefaultTypeMismatch { slot },
        } if context == execution_context && slot == invalid_slot
    ));
    assert_eq!(state, before);
}

#[test]
fn version_four_supported_scalar_slot_types_initialise() {
    for type_id in [
        orna_standard::BIGINT_TYPE_ID,
        orna_standard::FLOAT_TYPE_ID,
        orna_standard::BINARY_LARGE_OBJECT_TYPE_ID,
    ] {
        let slot_id = StateSlotId::from_bytes(type_id.to_bytes());
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            vec![orna_artifact::client_plan::StateSlot::new(
                slot_id,
                type_id,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Null,
            )],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::BOOLEAN_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        let mut state = super::super::ClientStateStore::new();

        let result = super::super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .expect("supported scalar state slot initialises");

        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
        assert_eq!(
            state
                .local()
                .get(&super::super::ClientStateKey::new(function, slot_id)),
            Some(
                &RuntimeValue::null(ResolvedType::value(type_id))
                    .expect("supported scalar null constructs"),
            ),
        );
    }
}

#[test]
fn version_four_unsupported_slot_type_fails_closed() {
    for type_id in [
        orna_standard::DATE_TYPE_ID,
        orna_standard::OPAQUE_TOKEN_TYPE_ID,
    ] {
        let slot_id = StateSlotId::from_bytes(type_id.to_bytes());
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            vec![orna_artifact::client_plan::StateSlot::new(
                slot_id,
                type_id,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Unset,
            )],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::BOOLEAN_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        let mut state = super::super::ClientStateStore::new();

        let error = super::super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            super::super::ClientExecutionError::StateEvaluation {
                context,
                source: super::super::ClientStateError::UnsupportedSlotType { slot },
            } if context.function() == function && *slot == slot_id
        ));
    }
}

#[test]
fn opaque_value_with_scalar_contract_is_not_a_supported_state_slot_type() {
    let definition = ValueTypeDefinition::opaque(
        TypeId::from_bytes([0xf2; 16]),
        QualifiedSemanticName::new(["tests", "opaque_scalar"]).unwrap(),
        "orna.kernel.value.boolean@1",
    );

    assert!(!super::super::state_slot_type_is_supported(&definition));
}

#[test]
fn version_four_return_type_mismatch_fails_as_an_expression_error() {
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Integer { value: 42 },
        vec![orna_artifact::client_plan::StateSlot::new(
            StateSlotId::from_bytes([0x51; 16]),
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::Local,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::super::ClientStateStore::new();

    let error = super::super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            context,
            source: super::super::ClientExpressionError::TypeMismatch,
        } if context.function() == function
    ));
}

#[test]
fn version_four_plans_run_through_the_legacy_entry_point_with_transient_state() {
    let (active, function, _) = version_four_text_state_plan();

    let result = evaluate_client_function(&active, function).unwrap();

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("hello world".to_owned())
    );
}

#[test]
fn procedural_literals_and_assignments_use_declaration_locals() {
    let local = LocalId::from_bytes([0xc1; 16]);
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            text_type,
            orna_artifact::client_plan::ClientLocalKind::Value,
        )],
        vec![
            orna_artifact::client_plan::ClientStatement::let_(
                local,
                orna_artifact::client_plan::ClientExpressionNode::String {
                    value: "first".to_owned(),
                },
            ),
            orna_artifact::client_plan::ClientStatement::assignment(
                local,
                orna_artifact::client_plan::ClientExpressionNode::String {
                    value: "second".to_owned(),
                },
            ),
        ],
        orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, _) = version_five_expression_active_with_parameter(payload);
    let grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xb1; 16]),
        RuntimeValue::Text("/tmp".to_owned()),
    )
    .unwrap();
    let result = super::super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &grants,
    )
    .unwrap();
    assert_eq!(result.value(), &RuntimeValue::Text("second".to_owned()));
}

#[test]
fn resource_request_rejects_missing_target_arguments() {
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Boolean(
            orna_artifact::client_plan::ClientPlan::return_boolean(false),
        ),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, _, _, _, _) = version_five_expression_active_with_parameter(payload);
    let digest = super::super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(FunctionId::from_bytes([0xd1; 16]), pair),
        PrincipalId::from_bytes([0x71; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut resource = super::super::ClientResource::new(key, ResolvedType::Value(text_type));

    let error = resource.begin_request(&active, Vec::new()).unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientResourceError::MissingArgument { parameter }
            if parameter == ParameterId::from_bytes([0xd3; 16])
    ));
}

#[test]
fn resource_request_rejects_unknown_target_arguments_before_loading() {
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Boolean(
            orna_artifact::client_plan::ClientPlan::return_boolean(false),
        ),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, _, _, _, _) = version_five_expression_active_with_parameter(payload);
    let digest = super::super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(FunctionId::from_bytes([0xd1; 16]), pair),
        PrincipalId::from_bytes([0x71; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut resource = super::super::ClientResource::new(key, ResolvedType::Value(text_type));
    let parameter = ParameterId::from_bytes([0xde; 16]);
    let argument = FunctionArgument::new(parameter, RuntimeValue::Text("/tmp".to_owned())).unwrap();

    let error = resource.begin_request(&active, vec![argument]).unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientResourceError::UnknownArgument { parameter: actual }
            if actual == parameter
    ));
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Idle);
    assert_eq!(resource.generation().value(), 0);
}

#[test]
fn procedural_await_without_executor_fails_closed() {
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        orna_artifact::client_plan::ResourceKind::Scalar,
        FunctionId::from_bytes([0xd1; 16]),
        pair,
        orna_core::CallSiteId::from_bytes([8; 16]),
        vec![(
            ParameterId::from_bytes([0xd3; 16]),
            orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0xb1; 16]),
            },
        )],
        text_type,
    );
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        Vec::new(),
        Vec::new(),
        orna_artifact::client_plan::ClientExpressionNode::Await {
            expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource {
                operation,
            }),
        },
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, _) = version_five_expression_active_with_parameter(payload);
    let grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xb1; 16]),
        RuntimeValue::Text("/tmp".to_owned()),
    )
    .unwrap();
    let error = super::super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &grants,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::ExecutorUnavailable,
            ..
        }
    ));
}

#[test]
fn procedural_scalar_resource_local_awaits_through_assignment_with_executor_value() {
    let local = LocalId::from_bytes([0xc2; 16]);
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let target_revision = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let target = FunctionId::from_bytes([0xd1; 16]);
    let parent_invocation_id = orna_core::InvocationId::from_bytes([0x91; 16]);
    let call_site_id = orna_core::CallSiteId::from_bytes([0x82; 16]);
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        orna_artifact::client_plan::ResourceKind::Scalar,
        target,
        target_revision,
        call_site_id,
        vec![(
            ParameterId::from_bytes([0xd3; 16]),
            orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0xb1; 16]),
            },
        )],
        text_type,
    );
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            text_type,
            orna_artifact::client_plan::ClientLocalKind::Resource(
                orna_artifact::client_plan::ResourceKind::Scalar,
            ),
        )],
        vec![
            orna_artifact::client_plan::ClientStatement::let_(
                local,
                orna_artifact::client_plan::ClientExpressionNode::Resource {
                    operation: operation.clone(),
                },
            ),
            orna_artifact::client_plan::ClientStatement::assignment(
                local,
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        ],
        orna_artifact::client_plan::ClientExpressionNode::Await {
            expression: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        },
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, parameter) =
        version_five_expression_active_with_parameter(payload);
    let grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(parameter, RuntimeValue::Text("/tmp".to_owned())).unwrap();
    let state_context = super::super::ClientStateContext::new(
        function,
        "profile-a".to_owned(),
        "instance-a".to_owned(),
    )
    .unwrap();
    let mut state = super::super::ClientStateStore::new();
    state.set_context(state_context);
    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |request: &super::super::ClientResourceRequest| {
            assert_eq!(
                request.invocation_context(),
                Some(super::super::ClientResourceInvocationContext::new(
                    parent_invocation_id,
                    call_site_id,
                    "profile-a".to_owned(),
                    "instance-a".to_owned(),
                )),
            );
            assert_eq!(request.key().target(), InvocationTarget::new(target, pair));
            assert_eq!(request.arguments().len(), 1);
            assert_eq!(
                request.arguments()[0].parameter(),
                ParameterId::from_bytes([0xd3; 16]),
            );
            assert_eq!(
                request.arguments()[0].value(),
                &RuntimeValue::Text("/tmp".to_owned()),
            );
            Ok(RuntimeValue::Text("executor-value".to_owned()))
        },
    );

    let result = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &state.context().clone(),
        &[argument],
        &[],
        &grants,
        &mut state,
        parent_invocation_id,
        &mut executor,
    )
    .unwrap();

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("executor-value".to_owned())
    );
}

#[test]
fn evaluator_resource_key_includes_host_data_invalidation_token() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/data-token".to_owned())).unwrap();
    let context_a = super::super::ClientStateContext::new_with_data_invalidation_token(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
        Sha256Digest::from_bytes([0x11; 32]),
    )
    .unwrap();
    let context_b = super::super::ClientStateContext::new_with_data_invalidation_token(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
        Sha256Digest::from_bytes([0x12; 32]),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor = RecordingActionExecutor::new(None);
    let pending_key = |error: super::super::ClientExecutionError| match error {
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Pending { key, .. },
            ..
        } => key,
        other => panic!("expected pending resource evaluation, got {other:?}"),
    };
    let key_a = pending_key(
        super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context_a,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x31; 16]),
            &mut executor,
        )
        .unwrap_err(),
    );
    let key_b = pending_key(
        super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context_b,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x32; 16]),
            &mut executor,
        )
        .unwrap_err(),
    );

    assert_ne!(
        key_a, key_b,
        "host data invalidation must select a new local key"
    );
    assert_eq!(
        executor.cancelled.len(),
        1,
        "the old loading generation is cancelled"
    );
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Idle)
    );
    assert_eq!(
        state.resource(key_b).map(ClientResource::status),
        Some(ClientResourceStatus::Loading)
    );
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
}

#[test]
fn evaluator_resource_key_includes_state_context_identity() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/state-context".to_owned()),
    )
    .unwrap();
    let context_a = super::super::ClientStateContext::new(
        function,
        "profile-a".to_owned(),
        "instance-a".to_owned(),
    )
    .unwrap();
    let context_b = super::super::ClientStateContext::new(
        FunctionId::from_bytes([0xa1; 16]),
        "profile-b".to_owned(),
        "instance-b".to_owned(),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor_a =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("context-a".to_owned())));
    let result_a = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &context_a,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xa2; 16]),
        &mut executor_a,
    )
    .unwrap();
    let key_a = executor_a.executed[0].key();
    assert_eq!(
        result_a.value(),
        &RuntimeValue::Text("context-a".to_owned())
    );
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );

    let mut executor_b =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("context-b".to_owned())));
    let result_b = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &context_b,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xa3; 16]),
        &mut executor_b,
    )
    .unwrap();
    let key_b = executor_b.executed[0].key();

    assert_ne!(
        key_a, key_b,
        "state context switch must select a new local key"
    );
    assert_eq!(
        executor_b.executed.len(),
        1,
        "the READY result must not be reused"
    );
    assert_eq!(
        result_b.value(),
        &RuntimeValue::Text("context-b".to_owned())
    );
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());
}

#[test]
fn evaluator_resource_key_changes_after_user_state_mutation() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/user-state".to_owned())).unwrap();
    let context = super::super::ClientStateContext::new(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor_a =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("before".to_owned())));
    let result_a = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &context,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xb1; 16]),
        &mut executor_a,
    )
    .unwrap();
    let key_a = executor_a.executed[0].key();
    assert_eq!(result_a.value(), &RuntimeValue::Text("before".to_owned()));
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );

    let user_key = super::super::ClientStateKey::from_context(
        &context,
        function,
        StateSlotId::from_bytes([0xb2; 16]),
    );
    state
        .set_user_state(
            user_key,
            RuntimeValue::Text("changed".to_owned()),
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        )
        .unwrap();

    let mut executor_b = RecordingActionExecutor::new(Some(RuntimeValue::Text("after".to_owned())));
    let result_b = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &context,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xb3; 16]),
        &mut executor_b,
    )
    .unwrap();
    let key_b = executor_b.executed[0].key();

    assert_ne!(
        key_a, key_b,
        "USER state mutation must select a new local key"
    );
    assert_eq!(
        executor_b.executed.len(),
        1,
        "the READY result must not be reused"
    );
    assert_eq!(result_b.value(), &RuntimeValue::Text("after".to_owned()));
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());
}

#[test]
fn evaluator_resource_key_changes_after_user_state_reload() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/user-reload".to_owned()))
            .unwrap();
    let context = super::super::ClientStateContext::new(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor_a =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("before".to_owned())));
    let result_a = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &context,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xc1; 16]),
        &mut executor_a,
    )
    .unwrap();
    let key_a = executor_a.executed[0].key();
    assert_eq!(result_a.value(), &RuntimeValue::Text("before".to_owned()));
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
    assert_eq!(state.user_state_epoch(), 0);

    let durable_key = UserStateKey::new(
        PrincipalId::from_bytes([0x7a; 16]),
        function,
        "profile".to_owned(),
        function,
        "instance".to_owned(),
        StateSlotId::from_bytes([0xc2; 16]),
    )
    .unwrap();
    state
        .load_user_state(&[UserStateCell::new(
            durable_key,
            RuntimeValue::Text("reloaded".to_owned()),
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    assert_eq!(state.user_state_epoch(), 1);

    let mut executor_b = RecordingActionExecutor::new(Some(RuntimeValue::Text("after".to_owned())));
    let result_b = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &context,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xc3; 16]),
        &mut executor_b,
    )
    .unwrap();
    let key_b = executor_b.executed[0].key();

    assert_ne!(
        key_a, key_b,
        "USER reload must select a new local cache identity"
    );
    assert_eq!(
        executor_b.executed.len(),
        1,
        "the READY result from before the reload must not be reused"
    );
    assert_eq!(result_b.value(), &RuntimeValue::Text("after".to_owned()));
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());
}

#[test]
fn evaluator_resource_key_includes_authorised_security_context() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let (direct_authorisation, role_authorisation) = authorise_with_role_context(pair, function);
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/security-context".to_owned()),
    )
    .unwrap();
    let context = super::super::ClientStateContext::new_with_data_invalidation_token(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
        Sha256Digest::from_bytes([0x21; 32]),
    )
    .unwrap();

    // A changed security context cannot reuse a READY value.
    let mut ready_state = ClientStateStore::new();
    let mut ready_executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("direct".to_owned())));
    let direct_result = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &direct_authorisation,
        &context,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut ready_state,
        InvocationId::from_bytes([0x41; 16]),
        &mut ready_executor,
    )
    .unwrap();
    let key_a = ready_executor.executed[0].key();
    assert_eq!(
        direct_result.value(),
        &RuntimeValue::Text("direct".to_owned())
    );
    assert_eq!(
        ready_state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );

    let mut role_executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("role".to_owned())));
    let role_result = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &role_authorisation,
        &context,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut ready_state,
        InvocationId::from_bytes([0x42; 16]),
        &mut role_executor,
    )
    .unwrap();
    let key_b = role_executor.executed[0].key();
    assert_ne!(key_a, key_b, "security context must select a new local key");
    assert_eq!(role_result.value(), &RuntimeValue::Text("role".to_owned()));
    assert_eq!(
        ready_state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
    assert_eq!(
        ready_state.resource(key_b).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());

    // The same security change also replaces an old loading generation and
    // routes cancellation through the caller-owned executor.
    let mut loading_state = ClientStateStore::new();
    let mut loading_executor = RecordingActionExecutor::new(None);
    let direct_error = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &direct_authorisation,
        &context,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut loading_state,
        InvocationId::from_bytes([0x43; 16]),
        &mut loading_executor,
    )
    .unwrap_err();
    let loading_key_a = match direct_error {
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Pending { key, .. },
            ..
        } => key,
        other => panic!("expected pending direct resource, got {other:?}"),
    };
    let role_error = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &role_authorisation,
        &context,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut loading_state,
        InvocationId::from_bytes([0x44; 16]),
        &mut loading_executor,
    )
    .unwrap_err();
    let loading_key_b = match role_error {
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Pending { key, .. },
            ..
        } => key,
        other => panic!("expected pending role resource, got {other:?}"),
    };
    assert_ne!(loading_key_a, loading_key_b);
    assert_eq!(loading_executor.cancelled.len(), 1);
    assert_eq!(
        loading_state
            .resource(loading_key_a)
            .map(ClientResource::status),
        Some(ClientResourceStatus::Idle)
    );
    assert_eq!(
        loading_state
            .resource(loading_key_b)
            .map(ClientResource::status),
        Some(ClientResourceStatus::Loading)
    );
}

#[test]
fn evaluator_resource_key_includes_security_snapshot_grants_without_reusing_ready() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let session_principal = PrincipalId::from_bytes([0x7a; 16]);
    let role = PrincipalId::from_bytes([0x7b; 16]);
    let principals = vec![
        Principal::new(
            session_principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        ),
        Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
    ];
    let memberships = vec![RoleMembership::new(role, session_principal)];
    let authorise_with_grants = |execute_grants| {
        let snapshot = SecuritySnapshot::new(
            pair,
            vec![function],
            principals.clone(),
            memberships.clone(),
            execute_grants,
        )
        .expect("security snapshot should validate");
        let session = snapshot
            .bind_authenticated_session(session_principal, vec![role])
            .expect("security session should bind");
        let ExecuteDecision::Allowed(authorisation) =
            snapshot.authorise_execute(&session, InvocationTarget::new(function, pair))
        else {
            panic!("direct grant should allow the function");
        };
        authorisation
    };
    let authorisation_a =
        authorise_with_grants(vec![ExecuteGrant::new(session_principal, function)]);
    let authorisation_b = authorise_with_grants(vec![
        ExecuteGrant::new(session_principal, function),
        ExecuteGrant::new(role, function),
    ]);

    assert_eq!(
        authorisation_a.session_principal(),
        authorisation_b.session_principal()
    );
    assert_eq!(
        authorisation_a.effective_principal(),
        authorisation_b.effective_principal()
    );
    assert_eq!(
        authorisation_a.authorising_principal(),
        authorisation_b.authorising_principal()
    );
    assert_eq!(
        authorisation_a.active_roles(),
        authorisation_b.active_roles()
    );
    assert_eq!(authorisation_a.target(), authorisation_b.target());

    let capabilities =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/security-snapshot-grants".to_owned()),
    )
    .unwrap();
    let context = super::super::ClientStateContext::new_with_data_invalidation_token(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
        Sha256Digest::from_bytes([0x21; 32]),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor_a =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("snapshot-a".to_owned())));
    let result_a = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorisation_a,
        &context,
        std::slice::from_ref(&argument),
        &[],
        &capabilities,
        &mut state,
        InvocationId::from_bytes([0x61; 16]),
        &mut executor_a,
    )
    .unwrap();
    let key_a = executor_a.executed[0].key();
    assert_eq!(
        result_a.value(),
        &RuntimeValue::Text("snapshot-a".to_owned())
    );
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );

    let mut executor_b =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("snapshot-b".to_owned())));
    let result_b = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorisation_b,
        &context,
        std::slice::from_ref(&argument),
        &[],
        &capabilities,
        &mut state,
        InvocationId::from_bytes([0x62; 16]),
        &mut executor_b,
    )
    .unwrap();
    let key_b = executor_b.executed[0].key();

    assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());
    assert_ne!(
        key_a, key_b,
        "snapshot grant changes must select a new local key"
    );
    assert_eq!(
        executor_b.executed.len(),
        1,
        "the READY result must not be reused"
    );
    assert_eq!(
        result_b.value(),
        &RuntimeValue::Text("snapshot-b".to_owned())
    );
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
    assert_eq!(
        state.resource(key_b).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
}

#[test]
fn ordinary_resource_pending_persists_only_the_loading_resource() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let state_context = super::super::ClientStateContext::new(
        FunctionId::from_bytes([0xa1; 16]),
        "profile".to_owned(),
        "instance".to_owned(),
    )
    .unwrap();
    let local_key = super::super::ClientStateKey::from_context(
        &state_context,
        function,
        StateSlotId::from_bytes([0xa2; 16]),
    );
    let session_key = super::super::ClientStateKey::from_context(
        &state_context,
        function,
        StateSlotId::from_bytes([0xa3; 16]),
    );
    let user_key = UserStateKey::new(
        principal,
        state_context.root_function(),
        state_context.state_profile().to_owned(),
        function,
        state_context.instance_key().to_owned(),
        StateSlotId::from_bytes([0xa4; 16]),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    state.set_context(state_context.clone());
    state
        .local_mut()
        .insert(local_key, RuntimeValue::Text("local".to_owned()));
    state
        .session_mut()
        .insert(session_key, RuntimeValue::Text("session".to_owned()));
    state
        .load_user_state(&[UserStateCell::new(
            user_key,
            RuntimeValue::Text("user".to_owned()),
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    let prior_context = state.context().clone();
    let prior_local = state.local().clone();
    let prior_session = state.session().clone();
    let prior_user = state.user().clone();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp/pending").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/pending".to_owned())).unwrap();
    let mut executor = RecordingActionExecutor::new(None);

    let error = super::super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0x91; 16]),
        &mut executor,
    )
    .unwrap_err();
    let (key, generation) = match error {
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Pending { key, generation },
            ..
        } => (key, generation),
        other => panic!("expected Pending resource evaluation, got {other:?}"),
    };

    assert_eq!(state.context(), &prior_context);
    assert_eq!(state.local(), &prior_local);
    assert_eq!(state.session(), &prior_session);
    assert_eq!(state.user(), &prior_user);
    let resource = state
        .resource(key)
        .expect("pending resource remains in caller state");
    let request_id = resource
        .request_id()
        .expect("pending resource has request identity");
    assert_eq!(resource.key(), key);
    assert_eq!(resource.generation(), generation);
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
    state
        .resource_mut(key)
        .expect("pending resource remains mutable in caller state")
        .apply_completion(
            &active,
            ClientResourceCompletion::Ready {
                request_id,
                key,
                generation,
                value: RuntimeValue::Text("resumed".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        state.resource(key).map(ClientResource::status),
        Some(ClientResourceStatus::Ready),
    );
}
#[test]
fn terminal_resource_states_persist_when_evaluation_fails() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/resource".to_owned())).unwrap();

    let mut failed_state = ClientStateStore::new();
    let mut failing_executor = FailingActionExecutor::default();
    let failure = super::super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut failed_state,
        InvocationId::from_bytes([0x92; 16]),
        &mut failing_executor,
    )
    .unwrap_err();

    assert!(matches!(
        failure,
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Failed(code),
            ..
        } if code == "secret.executor.detail"
    ));
    let failed_request = failing_executor
        .request
        .as_ref()
        .expect("failing executor received a resource request");
    let failed_resource = failed_state
        .resource(failed_request.key())
        .expect("failed resource remains at the evaluated request key");
    assert_eq!(failed_resource.key(), failed_request.key());
    assert_eq!(failed_resource.generation(), failed_request.generation());
    assert_eq!(
        failed_resource.request_id(),
        Some(failed_request.request_id()),
    );
    assert_eq!(failed_resource.status(), ClientResourceStatus::Failed);
    assert_eq!(
        failed_resource
            .failure()
            .map(super::super::ClientResourceFailure::code),
        Some("secret.executor.detail"),
    );

    let mut cancelled_state = ClientStateStore::new();
    let mut cancelled_executor = CancelledActionExecutor::default();
    let cancellation = super::super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut cancelled_state,
        InvocationId::from_bytes([0x93; 16]),
        &mut cancelled_executor,
    )
    .unwrap_err();

    assert!(matches!(
        cancellation,
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Cancelled,
            ..
        }
    ));
    let cancelled_request = cancelled_executor
        .request
        .as_ref()
        .expect("cancelled executor received a resource request");
    let cancelled_resource = cancelled_state
        .resource(cancelled_request.key())
        .expect("cancelled resource remains at the evaluated request key");
    assert_eq!(cancelled_resource.key(), cancelled_request.key());
    assert_eq!(
        cancelled_resource.generation(),
        cancelled_request.generation()
    );
    assert_eq!(
        cancelled_resource.request_id(),
        Some(cancelled_request.request_id()),
    );
    assert_eq!(cancelled_resource.status(), ClientResourceStatus::Cancelled);
    assert_eq!(cancelled_resource.failure(), None);
}

#[test]
fn same_revision_terminal_replacement_persists_when_new_evaluation_fails() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/replacement".to_owned()))
            .unwrap();
    let context = |token| {
        super::super::ClientStateContext::new_with_data_invalidation_token(
            function,
            "profile".to_owned(),
            "instance".to_owned(),
            Sha256Digest::from_bytes([token; 32]),
        )
        .unwrap()
    };

    for outcome in [
        ReplacementEvaluationOutcome::Pending,
        ReplacementEvaluationOutcome::Failed,
        ReplacementEvaluationOutcome::Invalid,
    ] {
        let mut state = ClientStateStore::new();
        let mut executor = ReplacementTerminalExecutor::new(outcome);
        let first_error = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context(0xa1),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xb1; 16]),
            &mut executor,
        )
        .unwrap_err();
        let old_key = match first_error {
            super::super::ClientExecutionError::ResourceEvaluation {
                source: super::super::ClientResourceExecutionError::Pending { key, .. },
                ..
            } => key,
            other => panic!("expected first resource request to remain pending, got {other:?}"),
        };
        assert_eq!(
            state.resource(old_key).map(ClientResource::status),
            Some(ClientResourceStatus::Loading),
        );

        let second_error = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context(0xa2),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xb2; 16]),
            &mut executor,
        )
        .unwrap_err();
        match (outcome, second_error) {
            (
                ReplacementEvaluationOutcome::Pending,
                super::super::ClientExecutionError::ResourceEvaluation {
                    source: super::super::ClientResourceExecutionError::Pending { .. },
                    ..
                },
            )
            | (
                ReplacementEvaluationOutcome::Failed,
                super::super::ClientExecutionError::ResourceEvaluation {
                    source: super::super::ClientResourceExecutionError::Failed(_),
                    ..
                },
            )
            | (
                ReplacementEvaluationOutcome::Invalid,
                super::super::ClientExecutionError::ResourceEvaluation {
                    source: super::super::ClientResourceExecutionError::Invalid(_),
                    ..
                },
            ) => {}
            (ReplacementEvaluationOutcome::Expression, _) => {
                unreachable!("expression outcome is covered by the dedicated regression")
            }
            (outcome, error) => {
                panic!("unexpected replacement evaluation result for {outcome:?}: {error:?}")
            }
        }

        let new_key = executor
            .executed
            .get(1)
            .expect("replacement request was submitted")
            .key();
        assert_ne!(old_key, new_key);
        assert_eq!(executor.cancelled[0].key(), old_key);
        let old_resource = state
            .resource(old_key)
            .expect("same-revision terminal replacement remains cached");
        assert_eq!(old_resource.status(), ClientResourceStatus::Ready);
        assert_eq!(
            old_resource.value(),
            Some(&RuntimeValue::Text("old-terminal".to_owned())),
        );
        match outcome {
            ReplacementEvaluationOutcome::Pending => assert_eq!(
                state.resource(new_key).map(ClientResource::status),
                Some(ClientResourceStatus::Loading),
            ),
            ReplacementEvaluationOutcome::Failed => assert_eq!(
                state.resource(new_key).map(ClientResource::status),
                Some(ClientResourceStatus::Failed),
            ),
            ReplacementEvaluationOutcome::Invalid => assert_eq!(
                state.resource(new_key).map(ClientResource::status),
                Some(ClientResourceStatus::Loading),
            ),
            ReplacementEvaluationOutcome::Expression => {
                unreachable!("expression outcome is covered by the dedicated regression")
            }
        }
    }
}

#[test]
fn same_revision_terminal_replacement_persists_when_later_expression_fails() {
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        orna_artifact::client_plan::ResourceKind::Scalar,
        FunctionId::from_bytes([0xd1; 16]),
        pair,
        CallSiteId::from_bytes([0xe1; 16]),
        vec![(
            ParameterId::from_bytes([0xd3; 16]),
            orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0xb1; 16]),
            },
        )],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let local = LocalId::from_bytes([0xf1; 16]);
    let return_expression = orna_artifact::client_plan::ClientExpressionNode::Concat {
        left: Box::new(orna_artifact::client_plan::ClientExpressionNode::LocalRead { local }),
        right: Box::new(orna_artifact::client_plan::ClientExpressionNode::Integer { value: 7 }),
    };
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            orna_artifact::client_plan::ClientLocalKind::Value,
        )],
        vec![orna_artifact::client_plan::ClientStatement::let_(
            local,
            orna_artifact::client_plan::ClientExpressionNode::Await {
                expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource {
                    operation,
                }),
            },
        )],
        return_expression,
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Parameter("p_path".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, parameter) =
        version_five_expression_active_with_parameter(payload);
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/later-error".to_owned()))
            .unwrap();
    let context = |token| {
        super::super::ClientStateContext::new_with_data_invalidation_token(
            function,
            "profile".to_owned(),
            "instance".to_owned(),
            Sha256Digest::from_bytes([token; 32]),
        )
        .unwrap()
    };
    let mut state = ClientStateStore::new();
    let mut executor = ReplacementTerminalExecutor::new(ReplacementEvaluationOutcome::Expression);
    let first_error = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &context(0xa3),
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xb3; 16]),
        &mut executor,
    )
    .unwrap_err();
    let old_key = match first_error {
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Pending { key, .. },
            ..
        } => key,
        other => panic!("expected first resource request to remain pending, got {other:?}"),
    };

    let second_error = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        &context(0xa4),
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xb4; 16]),
        &mut executor,
    )
    .unwrap_err();
    assert!(matches!(
        second_error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::TypeMismatch,
            ..
        }
    ));
    let new_key = executor
        .executed
        .get(1)
        .expect("replacement request was submitted")
        .key();
    assert_ne!(old_key, new_key);
    assert_eq!(executor.cancelled[0].key(), old_key);
    let old_resource = state
        .resource(old_key)
        .expect("same-revision terminal replacement remains cached");
    assert_eq!(old_resource.status(), ClientResourceStatus::Ready);
    assert_eq!(
        old_resource.value(),
        Some(&RuntimeValue::Text("old-terminal".to_owned())),
    );
    assert!(
        state.resource(new_key).is_none(),
        "failed outer expression must not publish its replacement resource"
    );
}

#[test]
fn stale_revision_replacement_persists_when_later_expression_fails() {
    let old_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let new_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([5; 16]),
        CatalogueRevisionId::from_bytes([6; 16]),
    );
    let payload = |pair| {
        let operation = orna_artifact::client_plan::ResourceOperationNode::new(
            orna_artifact::client_plan::ResourceKind::Scalar,
            FunctionId::from_bytes([0xd1; 16]),
            pair,
            CallSiteId::from_bytes([0xe1; 16]),
            vec![(
                ParameterId::from_bytes([0xd3; 16]),
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: ParameterId::from_bytes([0xb1; 16]),
                },
            )],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        let local = LocalId::from_bytes([0xf1; 16]);
        let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
            vec![orna_artifact::client_plan::ClientLocal::new(
                local,
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::ClientLocalKind::Value,
            )],
            vec![orna_artifact::client_plan::ClientStatement::let_(
                local,
                orna_artifact::client_plan::ClientExpressionNode::Await {
                    expression: Box::new(
                        orna_artifact::client_plan::ClientExpressionNode::Resource { operation },
                    ),
                },
            )],
            orna_artifact::client_plan::ClientExpressionNode::Concat {
                left: Box::new(
                    orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
                ),
                right: Box::new(orna_artifact::client_plan::ClientExpressionNode::Integer {
                    value: 7,
                }),
            },
        );
        orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                orna_artifact::client_plan::CapabilityArgumentSource::Parameter(
                    "p_path".to_owned(),
                ),
            )],
        )
        .encode()
        .unwrap()
    };
    let (old_active, function, _, _, parameter) =
        version_five_expression_active_with_parameter(payload(old_pair));
    let (new_base, _, _, _, _) = version_five_expression_active_with_parameter(payload(new_pair));
    let new_active = active_with_revision_pair(&new_base, new_pair);
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/stale-replacement".to_owned()),
    )
    .unwrap();
    let context = |token| {
        super::super::ClientStateContext::new_with_data_invalidation_token(
            function,
            "profile".to_owned(),
            "instance".to_owned(),
            Sha256Digest::from_bytes([token; 32]),
        )
        .unwrap()
    };
    let mut state = ClientStateStore::new();
    let mut executor = ReplacementTerminalExecutor::new(ReplacementEvaluationOutcome::Expression);
    let first_error = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &old_active,
        &authorise(old_pair, function),
        &context(0xa5),
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xb5; 16]),
        &mut executor,
    )
    .unwrap_err();
    let (old_key, old_generation) = match first_error {
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Pending { key, generation },
            ..
        } => (key, generation),
        other => panic!("expected first resource request to remain pending, got {other:?}"),
    };
    let old_request = executor
        .executed
        .first()
        .expect("old request was submitted")
        .clone();
    assert_eq!(old_request.key(), old_key);
    assert_eq!(old_request.generation(), old_generation);

    let second_error = super::super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        &new_active,
        &authorise(new_pair, function),
        &context(0xa6),
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0xb6; 16]),
        &mut executor,
    )
    .unwrap_err();
    assert!(matches!(
        second_error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::TypeMismatch,
            ..
        }
    ));
    let new_key = executor
        .executed
        .get(1)
        .expect("replacement request was submitted")
        .key();
    assert_ne!(old_key, new_key);
    assert_eq!(executor.cancelled, vec![old_request]);
    let old_resource = state
        .resource(old_key)
        .expect("stale replacement remains cached");
    assert_eq!(old_resource.status(), ClientResourceStatus::Idle);
    assert_eq!(old_resource.value(), None);
    assert_eq!(old_resource.failure(), None);
    assert_eq!(old_resource.request_id(), None);
    assert!(old_resource.generation().value() > old_generation.value());
    assert!(state.resource(new_key).is_none());
}

#[test]
fn malformed_resource_completion_cancels_executor_and_persists_terminal_state() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/malformed".to_owned())).unwrap();
    let mut state = ClientStateStore::new();
    let mut executor = MalformedResourceExecutor::default();

    let error = super::super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0x94; 16]),
        &mut executor,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Cancelled,
            ..
        }
    ));

    let request = executor
        .executed
        .clone()
        .expect("malformed executor received a resource request");
    assert_eq!(executor.cancelled, vec![request.clone()]);
    let mut resource = state
        .resource(request.key())
        .expect("cancelled resource remains in caller state")
        .clone();
    assert_eq!(resource.status(), ClientResourceStatus::Cancelled);
    assert_eq!(resource.generation(), request.generation());
    assert!(matches!(
        resource.apply_completion(
            &active,
            request.ready(RuntimeValue::Text("late".to_owned())),
        ),
        Err(super::super::ClientResourceError::InvalidTransition {
            status: ClientResourceStatus::Cancelled,
        })
    ));
}

#[test]
fn mismatched_request_id_completion_does_not_cancel_request() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/stale-request".to_owned()),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor = MalformedResourceExecutor {
        stale_request_id: true,
        ..MalformedResourceExecutor::default()
    };

    let error = super::super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0x96; 16]),
        &mut executor,
    )
    .expect_err("a mismatched request ID must not cancel the active request");
    let request = executor
        .executed
        .clone()
        .expect("executor received a resource request");
    assert!(executor.cancelled.is_empty());
    assert!(matches!(
        error,
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Invalid(_),
            ..
        }
    ));
    let resource = state
        .resource(request.key())
        .expect("the active request remains in caller state");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
}

#[test]
fn malformed_resource_completion_returns_terminal_cancel_result() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/malformed-ready".to_owned()),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor = MalformedResourceExecutor {
        cancel_ready: true,
        ..MalformedResourceExecutor::default()
    };

    let result = super::super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
        &active,
        &authorise(pair, function),
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        InvocationId::from_bytes([0x95; 16]),
        &mut executor,
    )
    .expect("valid terminal cancellation completion wins over malformed execute result");
    assert_eq!(
        result.value(),
        &RuntimeValue::Text("cancelled-ready".to_owned())
    );
    let request = executor
        .executed
        .clone()
        .expect("malformed executor received a resource request");
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(
        state.resource(request.key()).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
}

#[test]
fn procedural_scalar_resource_local_await_without_executor_fails_closed() {
    let local = LocalId::from_bytes([0xc3; 16]);
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        orna_artifact::client_plan::ResourceKind::Scalar,
        FunctionId::from_bytes([0xd1; 16]),
        pair,
        orna_core::CallSiteId::from_bytes([0x83; 16]),
        vec![(
            ParameterId::from_bytes([0xd3; 16]),
            orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0xb1; 16]),
            },
        )],
        text_type,
    );
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            text_type,
            orna_artifact::client_plan::ClientLocalKind::Resource(
                orna_artifact::client_plan::ResourceKind::Scalar,
            ),
        )],
        vec![orna_artifact::client_plan::ClientStatement::let_(
            local,
            orna_artifact::client_plan::ClientExpressionNode::Resource { operation },
        )],
        orna_artifact::client_plan::ClientExpressionNode::Await {
            expression: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        },
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, parameter) =
        version_five_expression_active_with_parameter(payload);
    let grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(parameter, RuntimeValue::Text("/tmp".to_owned())).unwrap();

    let error = super::super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &grants,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::ExecutorUnavailable,
            ..
        }
    ));
}
