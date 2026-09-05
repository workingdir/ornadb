//! Headless runtime ABI conformance tests.

use super::*;

fn unmount(node: NodeHandle) -> UiOperation {
    UiOperation {
        kind: UiOperationKind::UnmountNode,
        as_: UiOperationArgs { unmount_node: node },
    }
}

fn bind_action(
    node: NodeHandle,
    event_name: StringView,
    action: ActionHandle,
    input_type: StringView,
) -> UiOperation {
    UiOperation {
        kind: UiOperationKind::BindAction,
        as_: UiOperationArgs {
            bind_action: BindAction {
                node,
                event_name,
                action,
                input_type,
            },
        },
    }
}
fn unbind_action(
    node: NodeHandle,
    event_name: StringView,
    action: ActionHandle,
    input_type: StringView,
) -> UiOperation {
    let mut operation = bind_action(node, event_name, action, input_type);
    operation.kind = UiOperationKind::UnbindAction;
    operation
}
fn child_operation(
    kind: UiOperationKind,
    parent: NodeHandle,
    slot: StringView,
    child: NodeHandle,
    ordinal: usize,
) -> UiOperation {
    UiOperation {
        kind,
        as_: UiOperationArgs {
            child: ChildOperation {
                parent,
                slot,
                child,
                ordinal,
            },
        },
    }
}

fn unsupported_operation(kind: UiOperationKind) -> UiOperation {
    UiOperation {
        kind,
        as_: UiOperationArgs { unmount_node: 0 },
    }
}

fn assert_unsupported_operation_preserves_surface_state(
    kind: UiOperationKind,
    title: &'static [u8],
) {
    let session = FixtureSession::new();
    let surface = session.create_surface(title);
    let node_alias = next_unreserved_alias_handle();
    let action_alias = next_unreserved_alias_handle();
    assert_eq!(
        session.apply(surface, &batch(1, &[mount(node_alias, 0, view(b"root"))])),
        StatusCode::Ok
    );
    assert_eq!(
        session.apply(
            surface,
            &batch(
                2,
                &[bind_action(
                    node_alias,
                    view(b"activate"),
                    action_alias,
                    view(b"bool"),
                )],
            ),
        ),
        StatusCode::Ok
    );
    let before_capture = session.capture(surface);
    let before_state = {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let runtime = guard
            .runtime
            .as_ref()
            .expect("fixture runtime should exist");
        let surface_state = runtime
            .surfaces
            .get(&surface)
            .expect("surface should be live");
        (
            surface_state.revision,
            surface_state.visible,
            surface_state.node_aliases.clone(),
            surface_state.action_aliases.clone(),
            surface_state.owned_handles.clone(),
            runtime.node_tokens.clone(),
            runtime.action_tokens.clone(),
            runtime.known_handles.clone(),
            runtime.retired_handles.clone(),
            runtime.allocated_nodes.clone(),
            runtime.allocated_actions.clone(),
        )
    };
    let before_callback_log = session.callback_log();
    let before_release_counts = session.release_counts();
    let before_unknown_releases = UNKNOWN_RELEASES.load(Ordering::SeqCst);
    let before_allocation_owners = ALLOCATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .keys()
        .copied()
        .collect::<HashSet<_>>();
    let before_reservations = HANDLE_RESERVATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let mixed_operations = [
        set_property(node_alias, view(b"status")),
        unsupported_operation(kind),
    ];
    assert_eq!(
        session.apply(surface, &batch(3, &mixed_operations)),
        StatusCode::Unsupported
    );
    assert_eq!(session.callback_log(), before_callback_log);
    assert_eq!(session.release_counts(), before_release_counts);
    assert_eq!(
        UNKNOWN_RELEASES.load(Ordering::SeqCst),
        before_unknown_releases
    );
    assert_eq!(
        ALLOCATIONS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .copied()
            .collect::<HashSet<_>>(),
        before_allocation_owners
    );
    assert_eq!(session.capture(surface), before_capture);
    let after_state = {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let runtime = guard
            .runtime
            .as_ref()
            .expect("fixture runtime should exist");
        let surface_state = runtime
            .surfaces
            .get(&surface)
            .expect("surface should be live");
        (
            surface_state.revision,
            surface_state.visible,
            surface_state.node_aliases.clone(),
            surface_state.action_aliases.clone(),
            surface_state.owned_handles.clone(),
            runtime.node_tokens.clone(),
            runtime.action_tokens.clone(),
            runtime.known_handles.clone(),
            runtime.retired_handles.clone(),
            runtime.allocated_nodes.clone(),
            runtime.allocated_actions.clone(),
        )
    };
    assert_eq!(after_state, before_state);
    assert_eq!(
        *HANDLE_RESERVATIONS
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        before_reservations
    );
}

fn frame_body(frame: &[u8]) -> &[u8] {
    assert!(frame.starts_with(b"ORNA-UI/1 "));
    assert!(frame.len() >= 14);
    let body_length = u32::from_be_bytes(
        frame[10..14]
            .try_into()
            .expect("frame length is four bytes"),
    );
    assert_eq!(frame.len(), 14 + body_length as usize);
    &frame[14..]
}

#[test]
fn loads_valid_fixture_and_rejects_incompatible_table_before_describe() {
    let serial = serial_lock();
    DESCRIBE_CALLS.store(0, Ordering::SeqCst);
    assert_eq!(validate_api(&FIXTURE_API), Ok(()));
    assert_eq!(DESCRIBE_CALLS.load(Ordering::SeqCst), 1);

    let mut incompatible = FIXTURE_API;
    incompatible.abi_major = 2;
    DESCRIBE_CALLS.store(0, Ordering::SeqCst);
    assert_eq!(validate_api(&incompatible), Err(LoadError::AbiMajor(2)));
    assert_eq!(DESCRIBE_CALLS.load(Ordering::SeqCst), 0);

    DESCRIBE_CALLS.store(0, Ordering::SeqCst);
    let session = FixtureSession::new_with_serial(serial);
    assert_eq!(DESCRIBE_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(
        unsafe { (FIXTURE_API.start_event_loop)(session.runtime) }.code,
        StatusCode::Ok
    );
}

#[test]
fn unsupported_restore_state_is_rejected_before_surface_or_handle_mutation() {
    let session = FixtureSession::new();
    let existing = session.create_surface(b"Existing surface");
    let before_capture = session.capture(existing);
    let before_handles = {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .runtime
            .as_ref()
            .expect("fixture runtime should exist");
        (
            state.surfaces.len(),
            state.known_handles.clone(),
            state.retired_handles.clone(),
        )
    };
    let restore = b"opaque restore bytes";
    let options = SurfaceCreateOptions {
        surface_kind: view(b"window"),
        title: view(b"Unsupported restore"),
        state_profile: view(b"local"),
        opaque_runtime_restore_state: BytesView {
            data: restore.as_ptr(),
            len: restore.len(),
        },
    };
    let sentinel = 0xdead_beef_u64;
    let mut output = sentinel;
    assert_eq!(
        unsafe { (FIXTURE_API.create_surface)(session.runtime, &options, &mut output) }.code,
        StatusCode::Unsupported
    );
    assert_eq!(output, sentinel);
    let after_handles = {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .runtime
            .as_ref()
            .expect("fixture runtime should exist");
        (
            state.surfaces.len(),
            state.known_handles.clone(),
            state.retired_handles.clone(),
        )
    };
    assert_eq!(after_handles, before_handles);
    assert_eq!(session.capture(existing), before_capture);
}

#[test]
fn invalid_surface_visibility_is_rejected_without_mutation() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Visibility");
    let before_capture = session.capture(surface);
    let before_state = {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .runtime
            .as_ref()
            .expect("fixture runtime should exist");
        let surface_state = state
            .surfaces
            .get(&surface)
            .expect("surface should be live");
        (
            surface_state.visible,
            state.known_handles.clone(),
            state.retired_handles.clone(),
        )
    };
    assert_eq!(
        unsafe { (FIXTURE_API.set_surface_visible)(session.runtime, surface, 2) }.code,
        StatusCode::InvalidArgument
    );
    assert_eq!(session.capture(surface), before_capture);
    let after_state = {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .runtime
            .as_ref()
            .expect("fixture runtime should exist");
        let surface_state = state
            .surfaces
            .get(&surface)
            .expect("surface should be live");
        (
            surface_state.visible,
            state.known_handles.clone(),
            state.retired_handles.clone(),
        )
    };
    assert_eq!(after_state, before_state);
    assert_eq!(
        unsafe { (FIXTURE_API.set_surface_visible)(session.runtime, surface, 1) }.code,
        StatusCode::Ok
    );
}

#[test]
fn stale_visibility_preserves_not_found_and_terminal_status() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Stale visibility");
    assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
    assert_eq!(
        unsafe { (FIXTURE_API.set_surface_visible)(session.runtime, surface, 2) }.code,
        StatusCode::InvalidArgument
    );
    assert_eq!(
        unsafe { (FIXTURE_API.set_surface_visible)(session.runtime, surface, 1) }.code,
        StatusCode::NotFound
    );

    let terminal_surface = session.create_surface(b"Terminal visibility");
    assert_eq!(session.shutdown(), StatusCode::Ok);
    assert_eq!(
        unsafe { (FIXTURE_API.set_surface_visible)(session.runtime, terminal_surface, 1) }.code,
        StatusCode::Failed
    );
}

#[test]
fn direct_destroy_before_shutdown_keeps_fixture_runtime_registered() {
    let session = FixtureSession::new();
    let runtime = session.runtime;

    unsafe { (FIXTURE_API.destroy)(runtime) };

    let guard = global().lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(
        guard.runtime.as_ref().map(|state| state.handle),
        Some(runtime)
    );
    assert_eq!(session.client.runtime.load(Ordering::SeqCst), runtime);
}

#[test]
fn direct_destroy_from_non_owner_thread_keeps_fixture_runtime_registered() {
    let session = FixtureSession::new();
    let runtime = session.runtime;

    thread::spawn(move || unsafe { (FIXTURE_API.destroy)(runtime) })
        .join()
        .expect("non-owner destroy call should join");

    let guard = global().lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(
        guard.runtime.as_ref().map(|state| state.handle),
        Some(runtime)
    );
    assert_eq!(session.client.runtime.load(Ordering::SeqCst), runtime);
}

#[test]
fn owner_destroy_after_shutdown_clears_global_runtime_and_client_handle() {
    let session = FixtureSession::new();
    let runtime = session.runtime;

    assert_eq!(session.shutdown(), StatusCode::Ok);
    assert_eq!(session.client.runtime.load(Ordering::SeqCst), runtime);

    unsafe { (FIXTURE_API.destroy)(runtime) };

    let guard = global().lock().unwrap_or_else(|error| error.into_inner());
    assert!(guard.runtime.is_none());
    assert_eq!(session.client.runtime.load(Ordering::SeqCst), 0);
}

#[test]
fn status_messages_have_stable_codes_and_reject_unstructured_text() {
    let codes = [
        StatusCode::Ok,
        StatusCode::InvalidArgument,
        StatusCode::Unsupported,
        StatusCode::NotFound,
        StatusCode::Busy,
        StatusCode::Cancelled,
        StatusCode::Failed,
        StatusCode::Internal,
        StatusCode::StaleRevision,
    ];
    for code in codes {
        let status = status(code, b"contains secret credentials");
        let message = unsafe { text(status.message) }.expect("status message should be valid");
        assert_eq!(message.as_bytes(), status_message(code));
        assert!(!message.contains("secret"));
        assert!(RuntimeState::valid_status(status));
    }
    let invalid = Status {
        code: StatusCode::Failed,
        message: view(b"raw request argument"),
    };
    assert!(!RuntimeState::valid_status(invalid));
}

#[test]
fn rejects_descriptor_with_wrong_contract_name() {
    static WRONG_NAME: ContractVersion = ContractVersion {
        name: view(b"std.ui.Other"),
        major: 1,
        minor: 0,
        features: ptr::null(),
        feature_count: 0,
    };
    let mut descriptor = DESCRIPTOR;
    descriptor.contracts = &WRONG_NAME;
    assert_eq!(
        validate_descriptor(&descriptor),
        Err(LoadError::Descriptor("contract name"))
    );
}

#[test]
fn rejects_duplicate_contracts_versions_unknown_features_and_bad_counts() {
    static DUPLICATES: [ContractVersion; 2] = [CONTRACT, CONTRACT];
    static BAD_VERSION: ContractVersion = ContractVersion {
        name: view(b"std.ui.UI"),
        major: 2,
        minor: 0,
        features: ptr::null(),
        feature_count: 0,
    };
    static DUPLICATE_FEATURES: [StringView; 2] = [view(b"accessibility"), view(b"accessibility")];
    static DUPLICATE_FEATURE_CONTRACT: ContractVersion = ContractVersion {
        name: view(b"std.ui.UI"),
        major: 1,
        minor: 0,
        features: DUPLICATE_FEATURES.as_ptr(),
        feature_count: DUPLICATE_FEATURES.len(),
    };

    let mut duplicate = DESCRIPTOR;
    duplicate.contracts = DUPLICATES.as_ptr();
    duplicate.contract_count = DUPLICATES.len();
    assert_eq!(
        validate_descriptor(&duplicate),
        Err(LoadError::Descriptor("contract count"))
    );

    let mut unsupported = DESCRIPTOR;
    unsupported.contracts = &BAD_VERSION;
    assert_eq!(
        validate_descriptor(&unsupported),
        Err(LoadError::Descriptor("contract version"))
    );
    let mut duplicate_features = DESCRIPTOR;
    duplicate_features.contracts = &DUPLICATE_FEATURE_CONTRACT;
    assert_eq!(
        validate_descriptor(&duplicate_features),
        Err(LoadError::Descriptor("contract features"))
    );

    let mut unknown_feature = DESCRIPTOR;
    unknown_feature.features = 1u64 << 63;
    assert_eq!(
        validate_descriptor(&unknown_feature),
        Err(LoadError::Descriptor("unknown feature"))
    );
    let mut malformed_thread_model = DESCRIPTOR;
    malformed_thread_model.thread_model = ThreadModel(99);
    assert_eq!(
        validate_descriptor(&malformed_thread_model),
        Err(LoadError::Descriptor("thread model"))
    );

    let mut malformed = DESCRIPTOR;
    malformed.sinks = ptr::null();
    assert_eq!(
        validate_descriptor(&malformed),
        Err(LoadError::Descriptor("sink count"))
    );
}

#[test]
fn rejects_unknown_operation_and_event_kinds() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Unknown kinds");
    let operations = [UiOperation {
        kind: UiOperationKind(99),
        as_: UiOperationArgs { unmount_node: 0 },
    }];
    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::InvalidArgument
    );

    let event = RuntimeEvent {
        kind: EventKind(99),
        as_: RuntimeEventArgs {
            diagnostic: DiagnosticEvent {
                status: status(StatusCode::Failed, b"unknown"),
            },
        },
    };
    assert_eq!(
        unsafe {
            client_emit_runtime_event(
                (&*session.client as *const ClientContext).cast_mut().cast(),
                session.runtime,
                &event,
            )
        }
        .code,
        StatusCode::InvalidArgument
    );
    assert!(session.callback_log().events.is_empty());
}

#[test]
fn rejects_oversized_batches_and_value_payloads() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Input limits");
    let operations = std::iter::repeat_with(|| set_property(1, view(b"property")))
        .take(MAX_BATCH_OPERATIONS + 1)
        .collect::<Vec<_>>();
    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::InvalidArgument
    );

    let mut invalid_mount = mount(1, 0, view(b"slot"));
    invalid_mount
        .as_
        .mount_node
        .explicit_key
        .canonical_encoding
        .len = MAX_VIEW_BYTES + 1;
    assert_eq!(
        session.apply(surface, &batch(1, &[invalid_mount])),
        StatusCode::InvalidArgument
    );
}
#[test]
fn rejects_nonzero_value_handles_at_operation_boundary() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Value handles");
    let mut invalid_mount = mount(0x1201, 0, view(b"root"));
    invalid_mount.as_.mount_node.explicit_key.handle = 1;

    assert_eq!(
        session.apply(surface, &batch(1, &[invalid_mount])),
        StatusCode::InvalidArgument
    );
    assert_eq!(
        frame_body(&session.capture(surface)),
        b"{\"kind\":\"empty\"}"
    );
}

#[test]
fn borrowed_batch_input_is_not_retained_and_capture_preserves_values() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Borrowed input");
    assert_eq!(
        frame_body(&session.capture(surface)),
        b"{\"kind\":\"empty\"}"
    );

    let mut slot = *b"slot";
    let slot_view = StringView {
        data: slot.as_mut_ptr().cast::<c_char>(),
        len: slot.len(),
    };
    let mut key = *b"first";
    let mut operation = mount(0xffff, 0, slot_view);
    operation.as_.mount_node.explicit_key = ValueRef {
        handle: 0,
        type_name: view(b"std.json.Value"),
        canonical_encoding: BytesView {
            data: key.as_ptr(),
            len: key.len(),
        },
    };
    assert_eq!(
        session.apply(surface, &batch(1, &[operation])),
        StatusCode::Ok
    );
    slot.copy_from_slice(b"xxxx");
    key.copy_from_slice(b"other");

    let captured = session.capture(surface);
    assert_eq!(
        frame_body(&captured),
        b"{\"actions\":{},\"call_site_id\":null,\"contract\":{\"id\":\"std.ui.UI\",\"name\":\"std.ui.UI\",\"version\":\"1.0\"},\"function_instance_id\":null,\"key\":{\"type\":\"std.json.Value\",\"value\":\"6669727374\"},\"kind\":\"node\",\"properties\":{},\"slots\":{}}",
    );
    assert!(captured.windows(4).any(|window| window == b"slot"));
    assert!(!captured.windows(4).any(|window| window == b"xxxx"));
    assert!(captured.windows(10).any(|window| window == b"6669727374"));
    assert!(!captured.windows(10).any(|window| window == b"6f74686572"));
    assert_eq!(session.release_counts(), (2, 0));
}
#[test]
fn owned_outputs_reject_changed_length_owner_and_double_release() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Owned outputs");
    let mut output = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe { (FIXTURE_API.capture_semantic_state)(session.runtime, surface, &mut output) }.code,
        StatusCode::Ok,
    );
    let baseline_unknown = UNKNOWN_RELEASES.load(Ordering::SeqCst);
    unsafe {
        (output.release)(output.owner, output.data, output.len + 1);
    }
    assert_eq!(session.release_counts(), (0, 1));
    unsafe {
        (output.release)(output.owner, output.data.wrapping_add(1), output.len);
    }
    assert_eq!(session.release_counts(), (0, 2));
    unsafe {
        (output.release)(output.owner, output.data, output.len);
    }
    assert_eq!(session.release_counts(), (1, 2));
    unsafe {
        (output.release)(output.owner, output.data, output.len);
    }
    assert_eq!(
        UNKNOWN_RELEASES.load(Ordering::SeqCst),
        baseline_unknown + 1
    );
    let mut opaque = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe { (FIXTURE_API.capture_opaque_state)(session.runtime, surface, &mut opaque) }.code,
        StatusCode::Ok,
    );
    assert!(opaque.data.is_null());
    assert_eq!(opaque.len, 0);
    assert!(!opaque.owner.is_null());
    unsafe {
        (opaque.release)(opaque.owner, opaque.data, opaque.len);
    }
    assert_eq!(session.release_counts(), (2, 2));

    let mut second = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe { (FIXTURE_API.capture_semantic_state)(session.runtime, surface, &mut second) }.code,
        StatusCode::Ok,
    );
    let wrong_owner = (second.owner as usize + 1) as *mut c_void;
    unsafe {
        (second.release)(wrong_owner, second.data, second.len);
    }
    assert_eq!(
        UNKNOWN_RELEASES.load(Ordering::SeqCst),
        baseline_unknown + 2
    );
    unsafe {
        (second.release)(second.owner, second.data, second.len);
    }
    assert_eq!(session.release_counts(), (3, 2));

    let mut unchanged = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe { (FIXTURE_API.capture_semantic_state)(session.runtime, 0xffff, &mut unchanged) }
            .code,
        StatusCode::InvalidArgument,
    );
    assert!(unchanged.data.is_null());
    assert_eq!(unchanged.len, 0);
    assert!(unchanged.owner.is_null());
}

#[test]
fn foreign_and_stale_handles_cannot_mutate_a_surface() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Handle provenance");
    let foreign = 0xffff_u64;
    assert_eq!(
        session.destroy_surface(foreign),
        StatusCode::InvalidArgument
    );
    let operations = [mount(9, foreign, view(b"root"))];
    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::InvalidArgument
    );
    assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
    assert_eq!(session.destroy_surface(surface), StatusCode::NotFound);

    let runtime = session.runtime;
    let cross_thread =
        thread::spawn(move || unsafe { (FIXTURE_API.poll_event_loop)(runtime, 0).code })
            .join()
            .expect("cross-thread probe should join");
    assert_eq!(cross_thread, StatusCode::Busy);
}

#[test]
fn zero_runtime_and_object_handles_return_canonical_invalid_argument() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Zero handles");

    assert_eq!(
        unsafe { (FIXTURE_API.start_event_loop)(0) }.code,
        StatusCode::InvalidArgument
    );
    assert_eq!(
        unsafe { (FIXTURE_API.destroy_surface)(session.runtime, 0) }.code,
        StatusCode::InvalidArgument
    );

    assert_eq!(
        session.apply(surface, &batch(1, &[mount(0, 0, view(b"root"))])),
        StatusCode::InvalidArgument
    );
    let node_token = next_unreserved_alias_handle();
    assert_eq!(
        session.apply(surface, &batch(1, &[mount(node_token, 0, view(b"root"))])),
        StatusCode::Ok
    );
    assert_eq!(
        session.apply(
            surface,
            &batch(
                2,
                &[bind_action(node_token, view(b"activate"), 0, view(b"bool"),)],
            ),
        ),
        StatusCode::InvalidArgument
    );

    let (model, request) = session.start_model_request(surface);
    assert_eq!(session.apply_model_rows(0), StatusCode::InvalidArgument);
    assert_eq!(session.cancel_request(0), StatusCode::InvalidArgument);
    assert_eq!(
        session.queue_event(RuntimeEvent {
            kind: EventKind::ModelRangeRequest,
            as_: RuntimeEventArgs {
                range_request: ModelRangeRequest {
                    request,
                    model: 0,
                    start: 0,
                    count: 1,
                    sort_filter_token: view(b"fixture"),
                },
            },
        }),
        StatusCode::InvalidArgument
    );
    assert_eq!(
        session.queue_event(RuntimeEvent {
            kind: EventKind::ModelRangeRequest,
            as_: RuntimeEventArgs {
                range_request: ModelRangeRequest {
                    request: 0,
                    model,
                    start: 0,
                    count: 1,
                    sort_filter_token: view(b"fixture"),
                },
            },
        }),
        StatusCode::InvalidArgument
    );
}

#[test]
fn generated_handles_are_nonzero_and_monotonic_within_one_runtime() {
    let session = FixtureSession::new();
    assert_ne!(session.runtime, 0);

    let first_surface = session.create_surface(b"First surface");
    let second_surface = session.create_surface(b"Second surface");
    assert_ne!(first_surface, 0);
    assert_ne!(second_surface, 0);
    assert!(first_surface < second_surface);

    let first_node_token = next_unreserved_alias_handle();
    assert_eq!(
        session.apply(
            first_surface,
            &batch(1, &[mount(first_node_token, 0, view(b"root"))]),
        ),
        StatusCode::Ok
    );
    let first_action_token = next_unreserved_alias_handle();
    assert_eq!(
        session.apply(
            first_surface,
            &batch(
                2,
                &[bind_action(
                    first_node_token,
                    view(b"activate"),
                    first_action_token,
                    view(b"bool"),
                )],
            ),
        ),
        StatusCode::Ok
    );
    let (first_node, first_action) = session.node_and_action(first_surface);

    let second_node_token = next_unreserved_alias_handle();
    assert_eq!(
        session.apply(
            second_surface,
            &batch(1, &[mount(second_node_token, 0, view(b"root"))]),
        ),
        StatusCode::Ok
    );
    let second_action_token = next_unreserved_alias_handle();
    assert_eq!(
        session.apply(
            second_surface,
            &batch(
                2,
                &[bind_action(
                    second_node_token,
                    view(b"activate"),
                    second_action_token,
                    view(b"bool"),
                )],
            ),
        ),
        StatusCode::Ok
    );
    let (second_node, second_action) = session.node_and_action(second_surface);

    assert_ne!(first_node, 0);
    assert_ne!(second_node, 0);
    assert!(first_node < second_node);
    assert_ne!(first_action, 0);
    assert_ne!(second_action, 0);
    assert!(first_action < second_action);

    let (first_model, first_request) = session.start_model_request(first_surface);
    let (second_model, second_request) = session.start_model_request(second_surface);
    assert_ne!(first_model, 0);
    assert_ne!(second_model, 0);
    assert!(first_model < second_model);
    assert_ne!(first_request, 0);
    assert_ne!(second_request, 0);
    assert!(first_request < second_request);
}

#[test]
fn caller_tokens_remain_provenant_across_surfaces_and_lifetimes() {
    let session = FixtureSession::new();
    let owner = session.create_surface(b"Token owner");
    let other = session.create_surface(b"Other surface");
    assert_eq!(
        session.apply(owner, &batch(1, &[mount(0x2001, 0, view(b"root"))])),
        StatusCode::Ok
    );
    assert_eq!(
        session.apply(other, &batch(1, &[mount(0x3001, 0, view(b"root"))])),
        StatusCode::Ok
    );
    assert_eq!(
        session.apply(other, &batch(2, &[mount(0x2001, 0, view(b"root"))])),
        StatusCode::InvalidArgument
    );
    assert_eq!(
        session.apply(
            owner,
            &batch(
                2,
                &[bind_action(
                    0x2001,
                    view(b"activate"),
                    0x2002,
                    view(b"bool")
                )]
            )
        ),
        StatusCode::Ok
    );
    assert_eq!(
        session.apply(
            other,
            &batch(
                2,
                &[bind_action(
                    0x3001,
                    view(b"activate"),
                    0x2002,
                    view(b"bool")
                )]
            )
        ),
        StatusCode::InvalidArgument
    );
    assert_eq!(
        session.apply(
            owner,
            &batch(
                3,
                &[unbind_action(
                    0x2001,
                    view(b"activate"),
                    0x2002,
                    view(b"bool")
                )]
            )
        ),
        StatusCode::Ok
    );
    assert_eq!(
        session.apply(
            owner,
            &batch(
                4,
                &[bind_action(
                    0x2001,
                    view(b"activate"),
                    0x2002,
                    view(b"bool")
                )]
            )
        ),
        StatusCode::NotFound
    );
    assert_eq!(
        session.apply(owner, &batch(4, &[unmount(0x2001)])),
        StatusCode::Ok
    );
    assert_eq!(
        session.apply(owner, &batch(5, &[mount(0x2001, 0, view(b"root"))])),
        StatusCode::NotFound
    );
    assert_eq!(session.destroy_surface(owner), StatusCode::Ok);
    assert_eq!(
        session.apply(other, &batch(2, &[mount(0x2001, 0, view(b"root"))])),
        StatusCode::NotFound
    );
}
#[test]
fn caller_aliases_reserve_future_generated_handles() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Alias reservation");
    let node_token = NEXT_HANDLE.load(Ordering::SeqCst);
    assert_eq!(
        session.apply(surface, &batch(1, &[mount(node_token, 0, view(b"root"))])),
        StatusCode::Ok
    );

    let action_token = NEXT_HANDLE.load(Ordering::SeqCst);
    assert_eq!(
        session.apply(
            surface,
            &batch(
                2,
                &[bind_action(
                    node_token,
                    view(b"activate"),
                    action_token,
                    view(b"bool")
                )]
            )
        ),
        StatusCode::Ok
    );
    let captured = session.capture(surface);
    let aliased_action_id = format!("\"action_id\":\"{action_token}\"");
    assert!(
        !captured
            .windows(aliased_action_id.len())
            .any(|window| { window == aliased_action_id.as_bytes() })
    );
    assert!(captured.windows(8).any(|window| window == b"activate"));
}

#[test]
fn reserved_handles_are_foreign_to_all_surface_operations() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Reserved handles");
    let foreign = next_unreserved_handle();
    let before = session.capture(surface);
    let operations = [
        unmount(foreign),
        set_property(foreign, view(b"title")),
        child_operation(
            UiOperationKind::InsertChild,
            foreign,
            view(b"slot"),
            0x7001,
            0,
        ),
        bind_action(foreign, view(b"submit"), 0x7002, view(b"std.json.Value")),
        unbind_action(foreign, view(b"submit"), 0x7002, view(b"std.json.Value")),
    ];

    for operation in operations {
        assert_eq!(
            session.apply(surface, &batch(1, std::slice::from_ref(&operation))),
            StatusCode::InvalidArgument
        );
        assert_eq!(session.capture(surface), before);
    }

    let mut reservations = HANDLE_RESERVATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(reservations.remove(&foreign));
}

#[test]
fn failed_batches_release_alias_and_generated_handle_reservations() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Failed reservations");
    let alias = NEXT_HANDLE.load(Ordering::SeqCst);
    let failed = [
        mount(alias, 0, view(b"root")),
        set_property(0x7fff, view(b"title")),
    ];

    assert_eq!(
        session.apply(surface, &batch(1, &failed)),
        StatusCode::NotFound
    );
    assert!(!is_reserved_handle(alias));
    assert!(!is_reserved_handle(alias + 1));
    assert_eq!(
        session.apply(surface, &batch(1, &[mount(alias, 0, view(b"root"))])),
        StatusCode::Ok
    );
}

#[test]
fn insert_child_rejects_an_already_mounted_child_without_moving_it() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Child ownership");
    assert_eq!(
        session.apply(surface, &batch(1, &[mount(0x4001, 0, view(b"root"))])),
        StatusCode::Ok
    );
    assert_eq!(
        session.apply(surface, &batch(2, &[mount(0x4002, 0x4001, view(b"child"))])),
        StatusCode::Ok
    );
    let before = session.capture(surface);
    let invalid_insert = [
        child_operation(
            UiOperationKind::InsertChild,
            0x4001,
            view(b"other"),
            0x4002,
            0,
        ),
        set_property(0x4002, view(b"title")),
    ];
    assert_eq!(
        session.apply(surface, &batch(3, &invalid_insert)),
        StatusCode::InvalidArgument
    );
    assert_eq!(session.capture(surface), before);
    assert_eq!(
        session.apply(
            surface,
            &batch(
                3,
                &[child_operation(
                    UiOperationKind::MoveChild,
                    0x4001,
                    view(b"other"),
                    0x4002,
                    0,
                )]
            )
        ),
        StatusCode::Ok
    );
    assert_eq!(
        session.apply(
            surface,
            &batch(
                4,
                &[child_operation(
                    UiOperationKind::InsertChild,
                    0x4001,
                    view(b"third"),
                    0x4002,
                    0,
                )]
            )
        ),
        StatusCode::InvalidArgument
    );
}

#[test]
fn valid_batches_are_atomic_and_revisions_are_deterministic() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Atomic batches");
    let operations = [
        mount(0x1001, 0, view(b"root")),
        set_property(0x1001, view(b"title")),
    ];
    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::Ok
    );
    let before = session.capture(surface);

    let malformed = [UiOperation {
        kind: UiOperationKind::UnmountNode,
        as_: UiOperationArgs {
            unmount_node: u64::MAX,
        },
    }];
    assert_eq!(
        session.apply(surface, &batch(2, &malformed)),
        StatusCode::NotFound
    );
    assert_eq!(session.capture(surface), before);
    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::StaleRevision
    );
    let second = [set_property(0x1001, view(b"status"))];
    assert_eq!(session.apply(surface, &batch(2, &second)), StatusCode::Ok);
    let after_second = session.capture(surface);
    assert_ne!(after_second, before);
    assert_eq!(
        session.apply(surface, &batch(2, &second)),
        StatusCode::StaleRevision
    );
    assert_eq!(
        session.apply(surface, &batch(4, &second)),
        StatusCode::InvalidArgument
    );
    assert_eq!(session.capture(surface), after_second);
}
#[test]
fn unsupported_set_focus_in_mixed_batch_preserves_revision_capture_aliases_and_reservations() {
    assert_unsupported_operation_preserves_surface_state(
        UiOperationKind::SetFocus,
        b"Unsupported focus",
    );
}

#[test]
fn unsupported_set_accessibility_in_mixed_batch_preserves_revision_capture_aliases_and_reservations()
 {
    assert_unsupported_operation_preserves_surface_state(
        UiOperationKind::SetAccessibility,
        b"Unsupported accessibility",
    );
}

#[test]
fn canonical_capture_escapes_json_control_characters() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"JSON escaping");
    let operations = [
        mount(0x7201, 0, view(b"root")),
        set_property(0x7201, view(b"\x08\x0c")),
    ];

    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::Ok
    );
    let frame = session.capture(surface);
    assert!(valid_canonical_frame(&frame));
    assert_eq!(
        frame_body(&frame),
        br#"{"actions":{},"call_site_id":null,"contract":{"id":"std.ui.UI","name":"std.ui.UI","version":"1.0"},"function_instance_id":null,"key":{"type":"std.json.Value","value":""},"kind":"node","properties":{"\b\f":{"type":"std.json.Value","value":""}},"slots":{}}"#
    );
}

#[test]
fn canonical_frame_validation_rejects_invalid_headers_lengths_and_values() {
    let valid =
        encode_surface_state(&[], &HashMap::new()).expect("empty semantic state should encode");
    assert!(valid_canonical_frame(&valid));

    let mut wrong_magic = valid.clone();
    wrong_magic[0] = b'X';
    assert!(!valid_canonical_frame(&wrong_magic));

    let mut wrong_length = valid.clone();
    wrong_length[13] -= 1;
    assert!(!valid_canonical_frame(&wrong_length));

    assert!(!valid_canonical_frame(b"ORNA-UI/1 \0\0\0\x08not json"));
    assert!(!valid_canonical_frame(
        b"ORNA-UI/1 \0\0\0\x0f{\"kind\":\"node\"}"
    ));
}
#[test]
fn canonical_frame_validation_accepts_minimal_nodes_and_optional_metadata() {
    let frame_for = |body: &[u8]| {
        let mut frame = Vec::from(b"ORNA-UI/1 ".as_slice());
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    };
    let minimal = frame_for(
        br#"{"actions":{},"contract":{"id":"std.ui.UI","name":"std.ui.UI","version":"1.0"},"kind":"node","properties":{},"slots":{}}"#,
    );
    assert!(valid_canonical_frame(&minimal));

    let with_optional = frame_for(
        br#"{"actions":{"activate":{"action_id":"action","debug_kind":null,"input_type":"bool","label":"Activate"}},"call_site_id":"call-site","contract":{"id":"std.ui.UI","name":"std.ui.UI","version":"1.0"},"function_instance_id":null,"key":null,"kind":"node","properties":{},"slots":{},"source_origin":{"source_unit_id":"unit"}}"#,
    );
    assert!(valid_canonical_frame(&with_optional));
}

#[test]
fn canonical_frame_validation_rejects_ui_value_over_node_bound() {
    let mut body = Vec::from(br#"{"children":["#.as_slice());
    for index in 0..MAX_RUNTIME_VALUE_NODES {
        if index != 0 {
            body.push(b',');
        }
        body.extend_from_slice(br#"{"kind":"empty"}"#);
    }
    body.extend_from_slice(br#"],"kind":"fragment"}"#);

    let mut frame = Vec::from(b"ORNA-UI/1 ".as_slice());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    assert!(!valid_canonical_frame(&frame));
}

#[test]
fn canonical_frame_validation_rejects_deeply_nested_invalid_ui_value() {
    const NESTING: usize = 60;
    let mut body = Vec::new();
    for _ in 0..NESTING {
        body.extend_from_slice(br#"{"children":["#);
    }
    body.extend_from_slice(br#"{"kind":"not-a-ui-kind"}"#);
    for _ in 0..NESTING {
        body.extend_from_slice(br#"],"kind":"fragment"}"#);
    }

    let mut frame = Vec::from(b"ORNA-UI/1 ".as_slice());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    assert!(!valid_canonical_frame(&frame));
}

#[test]
fn canonical_frame_validation_rejects_unknown_node_fields() {
    let body = br#"{"actions":{},"contract":{"id":"std.ui.UI","name":"std.ui.UI","version":"1.0"},"kind":"node","properties":{},"slots":{},"unexpected":true}"#;
    let mut frame = Vec::from(b"ORNA-UI/1 ".as_slice());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    assert!(!valid_canonical_frame(&frame));
}
#[test]
fn headless_fixture_validates_and_retains_canonical_ui_payload() {
    let session = HeadlessFixtureSession::new();
    let surface = session
        .create_surface()
        .expect("headless fixture surface should be created");
    let before_invalid_ui = session
        .capture(surface)
        .expect("empty surface capture should succeed");
    let invalid_ui = session
        .apply_ui_payload(b"not an ORNA-UI frame")
        .expect_err("headless fixture must reject invalid UI frames");
    assert_eq!(invalid_ui.status_code(), StatusCode::InvalidArgument);
    assert_eq!(invalid_ui.code(), StatusCode::InvalidArgument);
    assert_eq!(invalid_ui.kind(), HeadlessFixtureErrorKind::Validation);
    assert_eq!(
        invalid_ui.classification(),
        HeadlessFixtureErrorKind::Validation
    );
    assert_eq!(invalid_ui.to_string(), invalid_ui.message());
    assert!(invalid_ui.to_string().contains("ORNA-E100"));
    assert_eq!(
        session
            .capture(surface)
            .expect("invalid UI must not mutate the surface"),
        before_invalid_ui
    );

    let frame_for = |body: &[u8]| {
        let mut frame = Vec::from(b"ORNA-UI/1 ".as_slice());
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    };
    let body = br#"{"kind":"empty"}"#;
    let payload = frame_for(body);
    assert!(
        session
            .apply_ui_payload(&frame_for(br#"{ "kind": "empty" }"#))
            .is_err(),
        "headless fixture must reject non-canonical JSON whitespace"
    );
    assert!(
        session
            .apply_ui_payload(&frame_for(br#"{"kind":"fragment","children":[]}"#))
            .is_err(),
        "headless fixture must reject non-canonical JSON object key order"
    );
    let captured = session
        .apply_ui_payload(&payload)
        .expect("headless fixture should accept a canonical UI frame");
    assert!(valid_canonical_frame(&captured));
    let captured_body: serde_json::Value =
        serde_json::from_slice(frame_body(&captured)).expect("capture body should be JSON");
    let encoded_payload = payload
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        captured_body["properties"]["payload"]["type"],
        serde_json::Value::String("std.ui.UI".to_owned())
    );
    assert_eq!(
        captured_body["properties"]["payload"]["value"],
        serde_json::Value::String(encoded_payload)
    );
    session
        .destroy_surface(surface)
        .expect("headless fixture surface should be destroyed");
    session
        .shutdown()
        .expect("headless fixture should shut down");
    assert!(session.is_terminal());
    assert!(session.last_callback_is_terminal());
}

#[test]
fn headless_fixture_stale_handles_are_structured_and_non_mutating() {
    let session = HeadlessFixtureSession::new();
    let surface = session
        .create_surface()
        .expect("headless fixture surface should be created");
    let before = session
        .capture(surface)
        .expect("live surface capture should succeed");
    let foreign = next_unreserved_alias_handle();
    let foreign_error = session
        .capture(foreign)
        .expect_err("foreign handles must be rejected");
    assert_eq!(foreign_error.status_code(), StatusCode::InvalidArgument);
    assert_eq!(
        foreign_error.classification(),
        HeadlessFixtureErrorKind::Validation
    );
    assert_eq!(
        session.capture(surface).expect("surface remains live"),
        before
    );

    session
        .destroy_surface(surface)
        .expect("headless fixture surface should be destroyed");
    let stale_capture = session
        .capture(surface)
        .expect_err("destroyed handles must be rejected");
    assert_eq!(stale_capture.status_code(), StatusCode::NotFound);
    assert_eq!(stale_capture.kind(), HeadlessFixtureErrorKind::Lifecycle);
    assert_eq!(
        stale_capture.classification(),
        HeadlessFixtureErrorKind::Lifecycle
    );
    let stale_destroy = session
        .destroy_surface(surface)
        .expect_err("destroyed surface cannot be destroyed twice");
    assert_eq!(stale_destroy.status_code(), StatusCode::NotFound);
    assert_eq!(
        stale_destroy.classification(),
        HeadlessFixtureErrorKind::Lifecycle
    );
    session
        .shutdown()
        .expect("headless fixture should shut down");
}

#[test]
fn headless_fixture_model_cancellation_rejects_late_completion_structurally() {
    let session = HeadlessFixtureSession::new();
    session
        .create_surface()
        .expect("headless fixture surface should be created");
    let (_, request) = session
        .start_model_request()
        .expect("model request should be accepted");
    let surface = session
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .surface
        .expect("surface remains live before cancellation");
    let before_late_completion = session
        .capture(surface)
        .expect("capture before late completion should succeed");
    session
        .cancel_model_request(request)
        .expect("model request cancellation should succeed");
    let after_cancel = session.fixture.callback_log();
    let late_completion = session
        .complete_model_request(request)
        .expect_err("late completion must report cancellation");
    assert_eq!(late_completion.status_code(), StatusCode::Cancelled);
    assert_eq!(
        late_completion.kind(),
        HeadlessFixtureErrorKind::Cancellation
    );
    assert_eq!(
        late_completion.classification(),
        HeadlessFixtureErrorKind::Cancellation
    );
    assert_eq!(late_completion.to_string(), late_completion.message());
    assert_eq!(
        session
            .capture(surface)
            .expect("late completion is non-mutating"),
        before_late_completion
    );
    assert_eq!(session.fixture.callback_log(), after_cancel);
    session
        .destroy_surface(surface)
        .expect("surface should be destroyable after cancellation");
    session
        .shutdown()
        .expect("headless fixture should shut down");
}

#[test]
fn capture_rejects_a_corrupted_canonical_frame() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Corrupted frame");
    {
        let mut guard = global().lock().unwrap_or_else(|error| error.into_inner());
        guard
            .runtime
            .as_mut()
            .expect("fixture runtime should exist")
            .surfaces
            .get_mut(&surface)
            .expect("fixture surface should exist")
            .semantic = b"corrupted".to_vec();
    }
    let mut output = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe { fixture_capture_semantic_state(session.runtime, surface, &mut output) }.code,
        StatusCode::Internal,
    );
    assert!(output.data.is_null());
    assert_eq!(output.len, 0);
}

#[test]
fn callbacks_are_fifo_reentrant_calls_are_busy_and_requests_complete_once() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Callbacks");
    let request_surface = session.create_surface(b"Requests");
    session.set_reentry();
    assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
    let (model, request) = session.start_model_request(request_surface);
    assert_ne!(model, 0);
    assert_eq!(session.apply_model_rows(request), StatusCode::Ok);
    assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);
    let log = session.callback_log();
    assert_eq!(log.reentry_status, Some(StatusCode::Busy));
    assert_eq!(log.events[0].kind, EventKind::SurfaceClosed);
    assert_eq!(log.events[1].kind, EventKind::ModelRangeRequest);
    assert_eq!(log.events[1].request, request);
    assert_eq!(log.completions, vec![request]);
    assert_eq!(log.sequence.len(), 3);
    assert_eq!(log.sequence[0].sequence, 0);
    assert_eq!(
        log.sequence[0].kind,
        CallbackKind::Event(log.events[0].clone())
    );
    assert_eq!(log.sequence[1].sequence, 1);
    assert_eq!(
        log.sequence[1].kind,
        CallbackKind::Event(log.events[1].clone())
    );
    assert_eq!(log.sequence[2].sequence, 2);
    assert_eq!(log.sequence[2].kind, CallbackKind::Completion(request));
    assert!(log.sequence.iter().all(|record| !record.terminal));
}
#[test]
fn caller_pump_drains_queued_runtime_events_in_fifo_order() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Caller pumps");
    let (_, request) = session.start_model_request(surface);
    assert!(session.callback_log().events.is_empty());
    assert_eq!(session.poll(), StatusCode::Ok);
    let log = session.callback_log();
    assert_eq!(log.events.len(), 1);
    assert_eq!(log.events[0].kind, EventKind::ModelRangeRequest);
    assert_eq!(log.events[0].request, request);
    assert_eq!(log.sequence.len(), 1);
    assert_eq!(
        log.sequence[0].kind,
        CallbackKind::Event(log.events[0].clone())
    );
    assert!(!log.sequence[0].terminal);
}
#[test]
fn queued_events_copy_borrowed_payloads_before_caller_pump() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Borrowed events");
    let operations = [
        mount(0x7001, 0, view(b"root")),
        bind_action(0x7001, view(b"submit"), 0x7101, view(b"std.json.Value")),
    ];
    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::Ok
    );
    let (node, action) = session.node_and_action(surface);
    let mut payload = b"before".to_vec();
    let event = RuntimeEvent {
        kind: EventKind::Action,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface,
                node,
                action,
                payload: ValueRef {
                    handle: 0,
                    type_name: view(b"std.json.Value"),
                    canonical_encoding: BytesView {
                        data: payload.as_ptr(),
                        len: payload.len(),
                    },
                },
            },
        },
    };
    assert_eq!(session.queue_event(event), StatusCode::Ok);
    payload.copy_from_slice(b"after!");
    assert_eq!(session.poll(), StatusCode::Ok);
    assert_eq!(
        session.callback_log().action_payloads,
        vec![b"before".to_vec()]
    );
}

#[test]
fn typed_runtime_events_accept_all_declared_payloads() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Typed payloads");
    let operations = [
        mount(0x5001, 0, view(b"root")),
        bind_action(0x5001, view(b"submit"), 0x6001, view(b"std.json.Value")),
    ];
    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::Ok
    );
    let (node, action) = session.node_and_action(surface);
    let context = (&*session.client as *const ClientContext).cast_mut().cast();

    let action_event = RuntimeEvent {
        kind: EventKind::Action,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface,
                node,
                action,
                payload: empty_value(),
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &action_event) }.code,
        StatusCode::Ok
    );
    let mut wrong_type_payload = empty_value();
    wrong_type_payload.type_name = view(b"bool");
    let wrong_type_event = RuntimeEvent {
        kind: EventKind::Action,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface,
                node,
                action,
                payload: wrong_type_payload,
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &wrong_type_event) }.code,
        StatusCode::InvalidArgument
    );

    let mut foreign_value = empty_value();
    foreign_value.handle = 1;
    let foreign_value_event = RuntimeEvent {
        kind: EventKind::Action,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface,
                node,
                action,
                payload: foreign_value,
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &foreign_value_event) }.code,
        StatusCode::InvalidArgument
    );

    let focus_event = RuntimeEvent {
        kind: EventKind::FocusChanged,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface,
                node,
                action: 0,
                payload: empty_value(),
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &focus_event) }.code,
        StatusCode::Ok
    );

    let layout_event = RuntimeEvent {
        kind: EventKind::LayoutStateChanged,
        as_: RuntimeEventArgs {
            layout_state: LayoutStateEvent {
                surface,
                node,
                semantic_state_name: view(b"expanded"),
                semantic_state: empty_value(),
                opaque_runtime_state: BytesView {
                    data: ptr::null(),
                    len: 0,
                },
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &layout_event) }.code,
        StatusCode::Ok
    );

    let (model, request) = session.start_model_request(surface);
    assert_eq!(session.poll(), StatusCode::Ok);
    let children_event = RuntimeEvent {
        kind: EventKind::ModelChildrenRequest,
        as_: RuntimeEventArgs {
            children_request: ModelChildrenRequest {
                request,
                model,
                parent_key: empty_value(),
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &children_event) }.code,
        StatusCode::Ok
    );
    assert_eq!(session.emit_diagnostic(), StatusCode::Ok);
    assert_eq!(session.destroy_surface(surface), StatusCode::Ok);

    let log = session.callback_log();
    assert_eq!(
        log.events
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        vec![
            EventKind::Action,
            EventKind::FocusChanged,
            EventKind::LayoutStateChanged,
            EventKind::ModelRangeRequest,
            EventKind::ModelChildrenRequest,
            EventKind::Diagnostic,
            EventKind::SurfaceClosed,
        ]
    );
}
#[test]
fn callbacks_reject_foreign_requests_and_invalid_callback_values() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Callback validation");
    let (_, request) = session.start_model_request(surface);
    let context = (&*session.client as *const ClientContext).cast_mut().cast();
    let invalid_value = ValueRef {
        handle: 0,
        type_name: view(b""),
        canonical_encoding: BytesView {
            data: ptr::null(),
            len: 0,
        },
    };
    assert_eq!(
        unsafe { client_complete_model_request(context, request, invalid_value) }.code,
        StatusCode::InvalidArgument,
    );
    assert_eq!(
        unsafe {
            client_fail_model_request(
                context,
                request,
                Status {
                    code: StatusCode::Failed,
                    message: view(b"raw failure"),
                },
            )
        }
        .code,
        StatusCode::InvalidArgument,
    );
    session.fail_next_model_callback();
    assert_eq!(
        unsafe {
            client_fail_model_request(context, u64::MAX, status(StatusCode::Failed, b"foreign"))
        }
        .code,
        StatusCode::InvalidArgument,
    );
    assert_eq!(
        unsafe {
            client_fail_model_request(context, request, status(StatusCode::Failed, b"failure"))
        }
        .code,
        StatusCode::Failed,
    );
    let mut metadata = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe { client_read_action_metadata(context, 0xffff, &mut metadata) }.code,
        StatusCode::InvalidArgument,
    );
    let mut debug_json = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };

    assert_eq!(
        unsafe { client_read_value_debug_json(context, invalid_value, &mut debug_json) }.code,
        StatusCode::InvalidArgument,
    );
    assert!(session.callback_log().completions.is_empty());

    assert!(session.callback_log().failures.is_empty());

    assert_eq!(session.apply_model_rows(request), StatusCode::Ok);
    assert_eq!(
        unsafe { client_complete_model_request(context, request, empty_value()) }.code,
        StatusCode::NotFound,
    );
    assert_eq!(session.callback_log().completions, vec![request]);
}
#[test]
fn callbacks_reject_events_for_cancelled_requests() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Cancelled event");
    let (model, request) = session.start_model_request(surface);
    let context = (&*session.client as *const ClientContext).cast_mut().cast();
    assert_eq!(session.poll(), StatusCode::Ok);
    let event_count = session.callback_log().events.len();
    assert_eq!(session.cancel_request(request), StatusCode::Ok);

    let range = RuntimeEvent {
        kind: EventKind::ModelRangeRequest,
        as_: RuntimeEventArgs {
            range_request: ModelRangeRequest {
                request,
                model,
                start: 0,
                count: 1,
                sort_filter_token: view(b""),
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &range) }.code,
        StatusCode::NotFound
    );
    let children = RuntimeEvent {
        kind: EventKind::ModelChildrenRequest,
        as_: RuntimeEventArgs {
            children_request: ModelChildrenRequest {
                request,
                model,
                parent_key: empty_value(),
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &children) }.code,
        StatusCode::NotFound
    );
    assert_eq!(session.callback_log().events.len(), event_count);
}
#[test]
fn model_callbacks_record_one_terminal_outcome_per_request() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Callback outcomes");
    let (_, completed) = session.start_model_request(surface);
    let (_, failed) = session.start_model_request(surface);
    assert_eq!(session.poll(), StatusCode::Ok);
    let context = (&*session.client as *const ClientContext).cast_mut().cast();

    assert_eq!(
        unsafe { client_complete_model_request(context, completed, empty_value()) }.code,
        StatusCode::Ok
    );
    assert_eq!(
        unsafe { client_complete_model_request(context, completed, empty_value()) }.code,
        StatusCode::NotFound
    );
    assert_eq!(
        unsafe {
            client_fail_model_request(context, failed, status(StatusCode::Failed, b"fixture"))
        }
        .code,
        StatusCode::Ok
    );
    assert_eq!(
        unsafe { client_complete_model_request(context, failed, empty_value()) }.code,
        StatusCode::NotFound
    );

    let log = session.callback_log();
    assert_eq!(log.completions, vec![completed]);
    assert_eq!(log.failures, vec![(failed, StatusCode::Failed)]);
    assert_eq!(log.sequence.len(), 4);
    assert_eq!(log.sequence[2].kind, CallbackKind::Completion(completed));
    assert_eq!(
        log.sequence[3].kind,
        CallbackKind::Failure(failed, StatusCode::Failed)
    );
    assert!(!log.terminal);
}

#[test]
fn failed_surface_cancellation_retries_request_before_surface_close() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Failed cancellation");
    let (_, request) = session.start_model_request(surface);
    session.fail_next_model_callback();

    assert_eq!(session.destroy_surface(surface), StatusCode::Failed);
    assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
    assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);

    let log = session.callback_log();
    assert_eq!(log.failures, vec![(request, StatusCode::Cancelled)]);
    assert_eq!(
        log.events
            .iter()
            .filter(|event| event.kind == EventKind::SurfaceClosed)
            .count(),
        1
    );
}

#[test]
fn failed_direct_cancellation_preserves_request_until_callback_succeeds() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Failed direct cancellation");
    let (_, request) = session.start_model_request(surface);
    session.fail_next_model_callback();

    assert_eq!(session.cancel_request(request), StatusCode::Failed);
    assert_eq!(session.cancel_request(request), StatusCode::Ok);
    assert_eq!(session.apply_model_rows(request), StatusCode::Cancelled);
    assert_eq!(
        session.callback_log().failures,
        vec![(request, StatusCode::Cancelled)]
    );
}

#[test]
fn shutdown_retries_failed_request_cancellation_without_losing_request() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Shutdown retry");
    let (_, request) = session.start_model_request(surface);
    session.fail_next_model_callback();

    assert_eq!(session.shutdown(), StatusCode::Failed);
    let first = session.callback_log();
    assert!(first.failures.is_empty());
    assert!(!first.terminal);

    assert_eq!(session.shutdown(), StatusCode::Ok);
    let terminal = session.callback_log();
    assert_eq!(terminal.failures, vec![(request, StatusCode::Cancelled)]);
    assert_eq!(
        terminal
            .failures
            .iter()
            .filter(|(id, _)| *id == request)
            .count(),
        1
    );
    assert!(terminal.terminal);
    assert_eq!(
        terminal.sequence.last().map(|record| &record.kind),
        Some(&CallbackKind::Terminal)
    );

    let sequence = terminal.sequence.clone();
    assert_eq!(session.apply_model_rows(request), StatusCode::Failed);
    assert_eq!(session.callback_log().sequence, sequence);
}

#[test]
fn destroying_a_surface_cancels_its_pending_requests_once() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Request ownership");
    let (_, request) = session.start_model_request(surface);
    assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
    assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);
    assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);
    assert_eq!(session.destroy_surface(surface), StatusCode::NotFound);
    assert_eq!(
        session.callback_log().failures,
        vec![(request, StatusCode::Cancelled)]
    );
}

#[test]
fn destroying_a_surface_retires_all_owned_handles_and_suppresses_stale_work() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Owned handle retirement");
    let node_alias = next_unreserved_alias_handle();
    let action_alias = next_unreserved_alias_handle();
    let operations = [
        mount(node_alias, 0, view(b"root")),
        bind_action(
            node_alias,
            view(b"submit"),
            action_alias,
            view(b"std.json.Value"),
        ),
    ];
    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::Ok
    );
    let (node, action) = session.node_and_action(surface);
    let (model, request) = session.start_model_request(surface);
    let context = (&*session.client as *const ClientContext).cast_mut().cast();

    assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
    let after_destroy = session.callback_log();
    assert_eq!(
        after_destroy.failures,
        vec![(request, StatusCode::Cancelled)]
    );

    let stale_node_event = RuntimeEvent {
        kind: EventKind::FocusChanged,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface,
                node,
                action: 0,
                payload: empty_value(),
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &stale_node_event) }.code,
        StatusCode::NotFound
    );

    let mut metadata = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe { client_read_action_metadata(context, action, &mut metadata) }.code,
        StatusCode::NotFound
    );
    assert!(metadata.data.is_null());
    assert_eq!(metadata.len, 0);

    let stale_model_event = RuntimeEvent {
        kind: EventKind::ModelRangeRequest,
        as_: RuntimeEventArgs {
            range_request: ModelRangeRequest {
                request,
                model,
                start: 0,
                count: 1,
                sort_filter_token: view(b"fixture"),
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &stale_model_event) }.code,
        StatusCode::NotFound
    );
    assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);
    assert_eq!(session.cancel_request(request), StatusCode::NotFound);
    assert_eq!(
        unsafe { client_complete_model_request(context, request, empty_value()) }.code,
        StatusCode::NotFound
    );
    assert_eq!(session.capture_result(surface), Err(StatusCode::NotFound));
    assert_eq!(session.callback_log(), after_destroy);

    assert_eq!(session.shutdown(), StatusCode::Ok);
    assert!(session.callback_log().terminal);
}

#[test]
fn prior_runtime_handles_are_rejected_by_a_replacement_runtime_without_side_effects() {
    let first = FixtureSession::new();
    let old_runtime = first.runtime;
    let old_surface = first.create_surface(b"Prior runtime");
    let node_alias = next_unreserved_alias_handle();
    let action_alias = next_unreserved_alias_handle();
    let operations = [
        mount(node_alias, 0, view(b"root")),
        bind_action(
            node_alias,
            view(b"submit"),
            action_alias,
            view(b"std.json.Value"),
        ),
    ];
    assert_eq!(
        first.apply(old_surface, &batch(1, &operations)),
        StatusCode::Ok
    );
    let (old_node, old_action) = first.node_and_action(old_surface);
    let (old_model, old_request) = first.start_model_request(old_surface);
    assert_eq!(first.poll(), StatusCode::Ok);
    assert_eq!(first.shutdown(), StatusCode::Ok);
    unsafe { (FIXTURE_API.destroy)(old_runtime) };
    drop(first);

    let second = FixtureSession::new();
    assert_ne!(old_runtime, second.runtime);
    let replacement_surface = second.create_surface(b"Replacement runtime");
    let before_state = second.capture(replacement_surface);
    let before_log = second.callback_log();
    let context = (&*second.client as *const ClientContext).cast_mut().cast();

    let foreign_action_event = RuntimeEvent {
        kind: EventKind::Action,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface: old_surface,
                node: old_node,
                action: old_action,
                payload: empty_value(),
            },
        },
    };
    assert_eq!(
        unsafe { client_emit_runtime_event(context, old_runtime, &foreign_action_event) }.code,
        StatusCode::InvalidArgument
    );
    assert_eq!(
        unsafe { client_emit_runtime_event(context, second.runtime, &foreign_action_event) }.code,
        StatusCode::InvalidArgument
    );

    let foreign_focus_event = RuntimeEvent {
        kind: EventKind::FocusChanged,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface: old_surface,
                node: old_node,
                action: 0,
                payload: empty_value(),
            },
        },
    };
    let foreign_layout_event = RuntimeEvent {
        kind: EventKind::LayoutStateChanged,
        as_: RuntimeEventArgs {
            layout_state: LayoutStateEvent {
                surface: old_surface,
                node: old_node,
                semantic_state_name: view(b"fixture"),
                semantic_state: empty_value(),
                opaque_runtime_state: BytesView {
                    data: ptr::null(),
                    len: 0,
                },
            },
        },
    };
    let foreign_surface_closed_event = RuntimeEvent {
        kind: EventKind::SurfaceClosed,
        as_: RuntimeEventArgs {
            surface_closed: SurfaceClosedEvent {
                surface: old_surface,
            },
        },
    };
    let foreign_range_event = RuntimeEvent {
        kind: EventKind::ModelRangeRequest,
        as_: RuntimeEventArgs {
            range_request: ModelRangeRequest {
                request: old_request,
                model: old_model,
                start: 0,
                count: 1,
                sort_filter_token: view(b"fixture"),
            },
        },
    };
    let foreign_children_event = RuntimeEvent {
        kind: EventKind::ModelChildrenRequest,
        as_: RuntimeEventArgs {
            children_request: ModelChildrenRequest {
                request: old_request,
                model: old_model,
                parent_key: empty_value(),
            },
        },
    };
    for event in [
        &foreign_focus_event,
        &foreign_layout_event,
        &foreign_surface_closed_event,
        &foreign_range_event,
        &foreign_children_event,
    ] {
        assert_eq!(
            unsafe { client_emit_runtime_event(context, second.runtime, event) }.code,
            StatusCode::InvalidArgument
        );
    }

    let mut metadata = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe { client_read_action_metadata(context, old_action, &mut metadata) }.code,
        StatusCode::InvalidArgument
    );
    assert!(metadata.data.is_null());
    assert_eq!(metadata.len, 0);

    let mut opaque_state = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe {
            (FIXTURE_API.capture_opaque_state)(second.runtime, old_surface, &mut opaque_state)
        }
        .code,
        StatusCode::InvalidArgument
    );
    assert!(opaque_state.data.is_null());
    assert_eq!(opaque_state.len, 0);

    assert_eq!(
        unsafe { (FIXTURE_API.destroy_surface)(second.runtime, old_surface) }.code,
        StatusCode::InvalidArgument
    );
    assert_eq!(
        unsafe {
            (FIXTURE_API.apply_ui_batch)(second.runtime, old_surface, &batch(1, &operations))
        }
        .code,
        StatusCode::InvalidArgument
    );
    assert_eq!(
        unsafe { (FIXTURE_API.set_surface_visible)(second.runtime, old_surface, 1) }.code,
        StatusCode::InvalidArgument
    );
    let mut semantic_state = OwnedBytes {
        data: ptr::null_mut(),
        len: 0,
        owner: ptr::null_mut(),
        release: release_owned,
    };
    assert_eq!(
        unsafe {
            (FIXTURE_API.capture_semantic_state)(second.runtime, old_surface, &mut semantic_state)
        }
        .code,
        StatusCode::InvalidArgument
    );
    assert!(semantic_state.data.is_null());
    assert_eq!(semantic_state.len, 0);

    assert_eq!(
        unsafe { client_complete_model_request(context, old_request, empty_value()) }.code,
        StatusCode::InvalidArgument
    );
    assert_eq!(
        unsafe {
            client_fail_model_request(
                context,
                old_request,
                status(StatusCode::Failed, b"foreign request"),
            )
        }
        .code,
        StatusCode::InvalidArgument
    );
    assert_eq!(
        unsafe { (FIXTURE_API.apply_model_rows)(second.runtime, old_request, empty_value()) }.code,
        StatusCode::InvalidArgument
    );
    assert_eq!(
        unsafe { (FIXTURE_API.cancel_request)(second.runtime, old_request) }.code,
        StatusCode::InvalidArgument
    );
    assert_eq!(second.callback_log(), before_log);
    assert_eq!(
        second.capture_result(replacement_surface),
        Ok(before_state.clone())
    );
    assert_eq!(second.poll(), StatusCode::Ok);
    assert_eq!(
        unsafe { (FIXTURE_API.set_surface_visible)(second.runtime, replacement_surface, 1) }.code,
        StatusCode::Ok
    );
    assert_eq!(second.capture_result(replacement_surface), Ok(before_state));
}

#[test]
fn cancellation_wins_late_completion_and_shutdown_is_terminal() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Shutdown");
    let (_, cancelled) = session.start_model_request(surface);
    assert_eq!(session.cancel_request(cancelled), StatusCode::Ok);
    assert_eq!(session.cancel_request(cancelled), StatusCode::Cancelled);
    assert_eq!(session.apply_model_rows(cancelled), StatusCode::Cancelled);
    let (_, pending) = session.start_model_request(surface);
    assert_eq!(session.shutdown(), StatusCode::Ok);
    assert_eq!(session.cancel_request(cancelled), StatusCode::Failed);
    assert_eq!(session.apply_model_rows(cancelled), StatusCode::Failed);
    assert_eq!(session.apply_model_rows(pending), StatusCode::Failed);
    assert_eq!(session.apply_model_rows(pending), StatusCode::Failed);
    assert_eq!(
        unsafe { (FIXTURE_API.poll_event_loop)(session.runtime, 0) }.code,
        StatusCode::Failed
    );
    let log = session.callback_log();
    assert_eq!(
        log.failures,
        vec![
            (cancelled, StatusCode::Cancelled),
            (pending, StatusCode::Cancelled)
        ]
    );
    assert!(
        log.events
            .iter()
            .any(|event| { event.kind == EventKind::SurfaceClosed && event.surface == surface })
    );
    assert!(log.terminal);
    assert_eq!(
        log.sequence.last().map(|record| &record.kind),
        Some(&CallbackKind::Terminal)
    );
    assert!(log.sequence.last().is_some_and(|record| record.terminal));
    assert!(
        log.sequence[..log.sequence.len() - 1]
            .iter()
            .all(|record| !record.terminal)
    );
    let context = (&*session.client as *const ClientContext).cast_mut().cast();
    let post_terminal = RuntimeEvent {
        kind: EventKind::Diagnostic,
        as_: RuntimeEventArgs {
            diagnostic: DiagnosticEvent {
                status: status(StatusCode::Failed, b"fixture diagnostic"),
            },
        },
    };
    let sequence = log.sequence.clone();
    assert_eq!(
        unsafe { client_emit_runtime_event(context, session.runtime, &post_terminal) }.code,
        StatusCode::Failed
    );
    assert_eq!(session.callback_log().sequence, sequence);
    assert_eq!(session.emit_diagnostic(), StatusCode::Failed);
    assert_eq!(session.callback_log().events, log.events);
}

#[test]
fn headless_terminal_shutdown_rejects_new_work_without_callbacks() {
    let session = HeadlessFixtureSession::new();
    session
        .create_surface()
        .expect("headless fixture surface should be created");
    session
        .shutdown()
        .expect("headless fixture shutdown should reach terminal state");
    assert!(session.is_terminal());
    assert!(session.last_callback_is_terminal());

    let after_shutdown = session.fixture.callback_log();
    let shutdown_error = session
        .shutdown()
        .expect_err("terminal fixture must reject repeated shutdown");
    assert_eq!(shutdown_error.status_code(), StatusCode::Failed);
    assert_eq!(shutdown_error.kind(), HeadlessFixtureErrorKind::Lifecycle);
    assert_eq!(session.fixture.callback_log(), after_shutdown);
    let create_error = session
        .create_surface()
        .expect_err("terminal fixture must reject new surfaces");
    assert_eq!(create_error.status_code(), StatusCode::Failed);
    assert_eq!(create_error.kind(), HeadlessFixtureErrorKind::Lifecycle);
    assert_eq!(
        create_error.classification(),
        HeadlessFixtureErrorKind::Lifecycle
    );
    let request_error = session
        .start_model_request()
        .expect_err("terminal fixture must reject new model requests");
    assert_eq!(request_error.status_code(), StatusCode::Failed);
    assert_eq!(
        request_error.classification(),
        HeadlessFixtureErrorKind::Lifecycle
    );
    assert_eq!(session.fixture.callback_log(), after_shutdown);
}

// The canonical ./spec is absent in this checkout; this follows the accepted
// test-only contract in docs/decisions/0076-runtime-headless-conformance.md.
#[test]
fn shutdown_live_surface_cancels_and_retires_every_handle_without_post_terminal_callbacks() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Live handle shutdown");
    let node_alias = next_unreserved_alias_handle();
    let action_alias = next_unreserved_alias_handle();
    let operations = [
        mount(node_alias, 0, view(b"root")),
        bind_action(
            node_alias,
            view(b"submit"),
            action_alias,
            view(b"std.json.Value"),
        ),
    ];
    assert_eq!(
        session.apply(surface, &batch(1, &operations)),
        StatusCode::Ok
    );
    assert_eq!(
        unsafe { (FIXTURE_API.set_surface_visible)(session.runtime, surface, 1) }.code,
        StatusCode::Ok
    );
    let before_capture = session.capture(surface);
    assert!(
        before_capture.starts_with(b"ORNA-UI/1 "),
        "live surface should have canonical state before shutdown"
    );
    let (node, action) = session.node_and_action(surface);
    let (model, request) = session.start_model_request(surface);
    let runtime_snapshot = || {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let runtime = guard
            .runtime
            .as_ref()
            .expect("fixture runtime should exist");
        (
            (runtime.handle, runtime.shutdown_requested, runtime.terminal),
            (
                runtime.surfaces.keys().copied().collect::<HashSet<_>>(),
                runtime
                    .requests
                    .iter()
                    .map(|(request, record)| (*request, record.surface, record._model))
                    .collect::<HashSet<_>>(),
                runtime.pending_events.len(),
                runtime
                    .cancelled_requests
                    .iter()
                    .map(|(request, record)| (*request, record.surface, record._model))
                    .collect::<HashSet<_>>(),
            ),
            (runtime.node_tokens.clone(), runtime.action_tokens.clone()),
            (
                runtime.known_handles.clone(),
                runtime.retired_handles.clone(),
                runtime.known_surfaces.clone(),
                runtime.known_nodes.clone(),
                runtime.known_actions.clone(),
                runtime.known_models.clone(),
                runtime.known_requests.clone(),
                runtime.allocated_nodes.clone(),
                runtime.allocated_actions.clone(),
            ),
        )
    };
    let context_snapshot = || {
        let handles = session
            .client
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (
            (
                handles.known_surfaces.clone(),
                handles.live_surfaces.clone(),
                handles.known_nodes.clone(),
                handles.live_nodes.clone(),
                handles.known_actions.clone(),
                handles.live_actions.clone(),
                handles.known_models.clone(),
                handles.live_models.clone(),
                handles.known_requests.clone(),
                handles.live_requests.clone(),
            ),
            (
                handles.node_surfaces.clone(),
                handles.action_surfaces.clone(),
                handles.action_input_types.clone(),
                handles.model_surfaces.clone(),
                handles.request_surfaces.clone(),
                handles.request_models.clone(),
                handles.terminal_requests.clone(),
            ),
        )
    };

    assert_eq!(session.shutdown(), StatusCode::Ok);
    let terminal = session.callback_log();
    assert_eq!(
        terminal
            .failures
            .iter()
            .filter(|(id, code)| *id == request && *code == StatusCode::Cancelled)
            .count(),
        1,
        "shutdown must cancel the pending request exactly once"
    );
    assert_eq!(
        terminal
            .events
            .iter()
            .filter(|event| { event.kind == EventKind::SurfaceClosed && event.surface == surface })
            .count(),
        1,
        "shutdown must emit one terminal close event"
    );
    assert!(terminal.terminal);
    assert_eq!(terminal.sequence.len(), 4);
    assert!(matches!(
        terminal.sequence[0].kind.clone(),
        CallbackKind::Event(EventRecord {
            kind: EventKind::ModelRangeRequest,
            surface: event_surface,
            request: event_request,
        }) if event_surface == surface && event_request == request
    ));
    assert_eq!(
        terminal.sequence[1].kind,
        CallbackKind::Failure(request, StatusCode::Cancelled)
    );
    assert!(matches!(
        terminal.sequence[2].kind.clone(),
        CallbackKind::Event(EventRecord {
            kind: EventKind::SurfaceClosed,
            surface: event_surface,
            request: 0,
        }) if event_surface == surface
    ));
    assert_eq!(
        terminal.sequence.last().map(|record| &record.kind),
        Some(&CallbackKind::Terminal)
    );
    assert!(
        terminal
            .sequence
            .last()
            .is_some_and(|record| record.terminal)
    );
    assert!(
        terminal
            .sequence
            .iter()
            .take(terminal.sequence.len().saturating_sub(1))
            .all(|record| !record.terminal)
    );
    {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let runtime = guard
            .runtime
            .as_ref()
            .expect("fixture runtime should exist");
        assert!(runtime.surfaces.is_empty());
        assert!(runtime.requests.is_empty());
        assert!(runtime.pending_events.is_empty());
        assert!(runtime.retired_handles.contains(&surface));
        assert!(runtime.retired_handles.contains(&node));
        assert!(runtime.retired_handles.contains(&action));
        assert!(runtime.retired_handles.contains(&model));
        assert!(runtime.retired_handles.contains(&request));
    }
    {
        let handles = session
            .client
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(handles.live_surfaces.is_empty());
        assert!(handles.live_nodes.is_empty());
        assert!(handles.live_actions.is_empty());
        assert!(handles.live_models.is_empty());
        assert!(handles.live_requests.is_empty());
    }
    let terminal_runtime = runtime_snapshot();
    let terminal_context = context_snapshot();
    let terminal_releases = session.release_counts();
    let terminal_unknown_releases = UNKNOWN_RELEASES.load(Ordering::SeqCst);
    let terminal_allocation_owners = ALLOCATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .keys()
        .copied()
        .collect::<HashSet<_>>();
    let assert_unchanged = || {
        assert_eq!(runtime_snapshot(), terminal_runtime);
        assert_eq!(context_snapshot(), terminal_context);
        assert_eq!(session.callback_log(), terminal);
        assert_eq!(session.release_counts(), terminal_releases);
        assert_eq!(
            UNKNOWN_RELEASES.load(Ordering::SeqCst),
            terminal_unknown_releases
        );
        assert_eq!(
            ALLOCATIONS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .keys()
                .copied()
                .collect::<HashSet<_>>(),
            terminal_allocation_owners
        );
    };

    let context = (&*session.client as *const ClientContext).cast_mut().cast();
    let before_stale_handles = terminal.clone();

    // Surface operations are terminal once shutdown has drained the live surface.
    for _ in 0..2 {
        assert_eq!(session.capture_result(surface), Err(StatusCode::Failed));
        let mut semantic_state = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe {
                (FIXTURE_API.capture_semantic_state)(session.runtime, surface, &mut semantic_state)
            }
            .code,
            StatusCode::Failed
        );
        assert!(semantic_state.data.is_null());
        assert_eq!(semantic_state.len, 0);
        assert!(semantic_state.owner.is_null());
        assert_eq!(
            semantic_state.release as usize,
            release_owned as *const () as usize
        );

        let mut opaque_state = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe {
                (FIXTURE_API.capture_opaque_state)(session.runtime, surface, &mut opaque_state)
            }
            .code,
            StatusCode::Failed
        );
        assert!(opaque_state.data.is_null());
        assert_eq!(opaque_state.len, 0);
        assert!(opaque_state.owner.is_null());
        assert_eq!(
            opaque_state.release as usize,
            release_owned as *const () as usize
        );
        assert_eq!(
            unsafe { (FIXTURE_API.set_surface_visible)(session.runtime, surface, 0) }.code,
            StatusCode::Failed
        );

        let stale_operations = [set_property(node, view(b"after-shutdown"))];
        let stale_batch = batch(2, &stale_operations);
        assert_eq!(
            unsafe { (FIXTURE_API.apply_ui_batch)(session.runtime, surface, &stale_batch) }.code,
            StatusCode::Failed
        );
        assert_eq!(session.apply(surface, &stale_batch), StatusCode::Failed);
        assert_eq!(
            unsafe { (FIXTURE_API.destroy_surface)(session.runtime, surface) }.code,
            StatusCode::Failed
        );
    }
    assert_unchanged();

    // Node, action, and surface callbacks reject their retired handles.
    let stale_action_event = RuntimeEvent {
        kind: EventKind::Action,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface,
                node,
                action,
                payload: empty_value(),
            },
        },
    };
    let stale_focus_event = RuntimeEvent {
        kind: EventKind::FocusChanged,
        as_: RuntimeEventArgs {
            action: ActionEvent {
                surface,
                node,
                action: 0,
                payload: empty_value(),
            },
        },
    };
    let stale_surface_closed_event = RuntimeEvent {
        kind: EventKind::SurfaceClosed,
        as_: RuntimeEventArgs {
            surface_closed: SurfaceClosedEvent { surface },
        },
    };
    for _ in 0..2 {
        for event in [
            &stale_action_event,
            &stale_focus_event,
            &stale_surface_closed_event,
        ] {
            assert_eq!(
                unsafe {
                    client_emit_runtime_event(
                        context,
                        session.runtime,
                        event as *const RuntimeEvent,
                    )
                }
                .code,
                StatusCode::NotFound
            );
        }
    }
    for _ in 0..2 {
        let mut metadata = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe { client_read_action_metadata(context, action, &mut metadata) }.code,
            StatusCode::NotFound
        );
        assert!(metadata.data.is_null());
        assert_eq!(metadata.len, 0);
        assert!(metadata.owner.is_null());
        assert_eq!(
            metadata.release as usize,
            release_owned as *const () as usize
        );
    }
    assert_unchanged();

    // Model and request handles are retired after their one cancellation outcome.
    let stale_model_event = RuntimeEvent {
        kind: EventKind::ModelRangeRequest,
        as_: RuntimeEventArgs {
            range_request: ModelRangeRequest {
                request,
                model,
                start: 0,
                count: 1,
                sort_filter_token: view(b"fixture"),
            },
        },
    };
    let stale_children_event = RuntimeEvent {
        kind: EventKind::ModelChildrenRequest,
        as_: RuntimeEventArgs {
            children_request: ModelChildrenRequest {
                request,
                model,
                parent_key: empty_value(),
            },
        },
    };
    for _ in 0..2 {
        for event in [&stale_model_event, &stale_children_event] {
            assert_eq!(
                unsafe {
                    client_emit_runtime_event(
                        context,
                        session.runtime,
                        event as *const RuntimeEvent,
                    )
                }
                .code,
                StatusCode::NotFound
            );
        }
        assert_eq!(session.apply_model_rows(request), StatusCode::Failed);
        assert_eq!(session.cancel_request(request), StatusCode::Failed);
        assert_eq!(
            unsafe { client_complete_model_request(context, request, empty_value()) }.code,
            StatusCode::Failed
        );
        assert_eq!(
            unsafe {
                client_fail_model_request(
                    context,
                    request,
                    status(StatusCode::Cancelled, b"request cancelled"),
                )
            }
            .code,
            StatusCode::Failed
        );
    }
    assert_unchanged();

    // No stale operation may append a callback after the terminal marker.
    assert_eq!(session.callback_log(), before_stale_handles);
}

#[test]
fn typed_diagnostic_and_surface_events_keep_provenance() {
    let session = FixtureSession::new();
    let surface = session.create_surface(b"Typed events");
    assert_eq!(session.emit_diagnostic(), StatusCode::Ok);
    assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
    let log = session.callback_log();
    assert_eq!(
        log.events[0],
        EventRecord {
            kind: EventKind::Diagnostic,
            surface: 0,
            request: 0,
        }
    );
    assert_eq!(
        log.events[1],
        EventRecord {
            kind: EventKind::SurfaceClosed,
            surface,
            request: 0,
        }
    );
}
