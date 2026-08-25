/*
 * Linux x86_64 cross-language checks for the test-only Rust ABI mirror.
 *
 * The expected values are the constants asserted by runtime_abi in
 * crates/orna-client/src/lib.rs. Keep this translation unit against the
 * canonical sibling header; it must not become a production ABI loader.
 */

#if !defined(__linux__) || !defined(__x86_64__)
#error "runtime ABI parity is defined only for Linux x86_64"
#endif

#include <stddef.h>
#include <stdint.h>

#include <spec/orna_runtime_abi_v1.h>

#define ORNA_ASSERT_SIZE(type, expected) \
    _Static_assert(sizeof(type) == (expected), #type " size")
#define ORNA_ASSERT_ALIGN(type, expected) \
    _Static_assert(_Alignof(type) == (expected), #type " alignment")
#define ORNA_ASSERT_OFFSET(type, field, expected) \
    _Static_assert(offsetof(type, field) == (expected), #type "." #field " offset")
#define ORNA_ASSERT_MEMBER_SIZE(type, field, expected) \
    _Static_assert(sizeof(((type *)0)->field) == (expected), #type "." #field " size")

_Static_assert(ORNA_RUNTIME_ABI_V1_MAJOR == 1u, "ABI major");
_Static_assert(ORNA_RUNTIME_ABI_V1_MINOR == 0u, "ABI minor");
_Static_assert(sizeof(uint8_t) == 1, "uint8_t width");
_Static_assert(sizeof(int32_t) == 4, "int32_t width");
_Static_assert(sizeof(uint32_t) == 4, "uint32_t width");
_Static_assert(sizeof(uint64_t) == 8, "uint64_t width");
_Static_assert(sizeof(size_t) == 8, "size_t width");
_Static_assert(sizeof(void *) == 8, "object pointer width");
_Static_assert(sizeof(void (*)(void)) == 8, "function pointer width");

ORNA_ASSERT_SIZE(OrnaHandle, 8);
ORNA_ASSERT_SIZE(OrnaRuntimeHandle, 8);
ORNA_ASSERT_SIZE(OrnaSurfaceHandle, 8);
ORNA_ASSERT_SIZE(OrnaNodeHandle, 8);
ORNA_ASSERT_SIZE(OrnaActionHandle, 8);
ORNA_ASSERT_SIZE(OrnaModelHandle, 8);
ORNA_ASSERT_SIZE(OrnaRequestHandle, 8);

ORNA_ASSERT_SIZE(OrnaStringView, 16);
ORNA_ASSERT_ALIGN(OrnaStringView, 8);
ORNA_ASSERT_OFFSET(OrnaStringView, data, 0);
ORNA_ASSERT_OFFSET(OrnaStringView, len, 8);

ORNA_ASSERT_SIZE(OrnaBytesView, 16);
ORNA_ASSERT_ALIGN(OrnaBytesView, 8);
ORNA_ASSERT_OFFSET(OrnaBytesView, data, 0);
ORNA_ASSERT_OFFSET(OrnaBytesView, len, 8);

ORNA_ASSERT_SIZE(OrnaOwnedBytes, 32);
ORNA_ASSERT_ALIGN(OrnaOwnedBytes, 8);
ORNA_ASSERT_OFFSET(OrnaOwnedBytes, data, 0);
ORNA_ASSERT_OFFSET(OrnaOwnedBytes, len, 8);
ORNA_ASSERT_OFFSET(OrnaOwnedBytes, owner, 16);
ORNA_ASSERT_OFFSET(OrnaOwnedBytes, release, 24);

ORNA_ASSERT_SIZE(OrnaStatusCode, 4);
ORNA_ASSERT_ALIGN(OrnaStatusCode, 4);
_Static_assert(ORNA_STATUS_OK == 0, "ORNA_STATUS_OK");
_Static_assert(ORNA_STATUS_INVALID_ARGUMENT == 1, "ORNA_STATUS_INVALID_ARGUMENT");
_Static_assert(ORNA_STATUS_UNSUPPORTED == 2, "ORNA_STATUS_UNSUPPORTED");
_Static_assert(ORNA_STATUS_NOT_FOUND == 3, "ORNA_STATUS_NOT_FOUND");
_Static_assert(ORNA_STATUS_BUSY == 4, "ORNA_STATUS_BUSY");
_Static_assert(ORNA_STATUS_CANCELLED == 5, "ORNA_STATUS_CANCELLED");
_Static_assert(ORNA_STATUS_FAILED == 6, "ORNA_STATUS_FAILED");
_Static_assert(ORNA_STATUS_INTERNAL == 7, "ORNA_STATUS_INTERNAL");
_Static_assert(ORNA_STATUS_STALE_REVISION == 8, "ORNA_STATUS_STALE_REVISION");

ORNA_ASSERT_SIZE(OrnaStatus, 24);
ORNA_ASSERT_ALIGN(OrnaStatus, 8);
ORNA_ASSERT_OFFSET(OrnaStatus, code, 0);
ORNA_ASSERT_OFFSET(OrnaStatus, message, 8);

ORNA_ASSERT_SIZE(OrnaSurfaceClosedEventV1, 8);
ORNA_ASSERT_ALIGN(OrnaSurfaceClosedEventV1, 8);
ORNA_ASSERT_OFFSET(OrnaSurfaceClosedEventV1, surface, 0);

ORNA_ASSERT_SIZE(OrnaDiagnosticEventV1, 24);
ORNA_ASSERT_ALIGN(OrnaDiagnosticEventV1, 8);
ORNA_ASSERT_OFFSET(OrnaDiagnosticEventV1, status, 0);

ORNA_ASSERT_SIZE(OrnaThreadModel, 4);
ORNA_ASSERT_ALIGN(OrnaThreadModel, 4);
_Static_assert(ORNA_THREAD_MODEL_CLIENT_EVENT_LOOP == 1, "ORNA_THREAD_MODEL_CLIENT_EVENT_LOOP");
_Static_assert(ORNA_THREAD_MODEL_RUNTIME_EVENT_LOOP == 2, "ORNA_THREAD_MODEL_RUNTIME_EVENT_LOOP");
_Static_assert(ORNA_THREAD_MODEL_CALLER_PUMPS == 3, "ORNA_THREAD_MODEL_CALLER_PUMPS");

ORNA_ASSERT_SIZE(OrnaRuntimeFeature, 4);
ORNA_ASSERT_ALIGN(OrnaRuntimeFeature, 4);
_Static_assert(ORNA_RUNTIME_FEATURE_MULTIPLE_WINDOWS == (1u << 0), "ORNA_RUNTIME_FEATURE_MULTIPLE_WINDOWS");
_Static_assert(ORNA_RUNTIME_FEATURE_ACCESSIBILITY == (1u << 1), "ORNA_RUNTIME_FEATURE_ACCESSIBILITY");
_Static_assert(ORNA_RUNTIME_FEATURE_CLIPBOARD == (1u << 2), "ORNA_RUNTIME_FEATURE_CLIPBOARD");
_Static_assert(ORNA_RUNTIME_FEATURE_DRAG_DROP == (1u << 3), "ORNA_RUNTIME_FEATURE_DRAG_DROP");
_Static_assert(ORNA_RUNTIME_FEATURE_NATIVE_MENUS == (1u << 4), "ORNA_RUNTIME_FEATURE_NATIVE_MENUS");
_Static_assert(ORNA_RUNTIME_FEATURE_PRINTING == (1u << 5), "ORNA_RUNTIME_FEATURE_PRINTING");
_Static_assert(ORNA_RUNTIME_FEATURE_OPAQUE_LAYOUT_STATE == (1u << 6), "ORNA_RUNTIME_FEATURE_OPAQUE_LAYOUT_STATE");

ORNA_ASSERT_SIZE(OrnaContractVersionV1, 40);
ORNA_ASSERT_ALIGN(OrnaContractVersionV1, 8);
ORNA_ASSERT_OFFSET(OrnaContractVersionV1, name, 0);
ORNA_ASSERT_OFFSET(OrnaContractVersionV1, major, 16);
ORNA_ASSERT_OFFSET(OrnaContractVersionV1, minor, 20);
ORNA_ASSERT_OFFSET(OrnaContractVersionV1, features, 24);
ORNA_ASSERT_OFFSET(OrnaContractVersionV1, feature_count, 32);

ORNA_ASSERT_SIZE(OrnaSinkOfferV1, 40);
ORNA_ASSERT_ALIGN(OrnaSinkOfferV1, 8);
ORNA_ASSERT_OFFSET(OrnaSinkOfferV1, type_name, 0);
ORNA_ASSERT_OFFSET(OrnaSinkOfferV1, media_types, 16);
ORNA_ASSERT_OFFSET(OrnaSinkOfferV1, media_type_count, 24);
ORNA_ASSERT_OFFSET(OrnaSinkOfferV1, supports_streaming, 32);
ORNA_ASSERT_OFFSET(OrnaSinkOfferV1, preference_rank, 36);

ORNA_ASSERT_SIZE(OrnaRuntimeDescriptorV1, 120);
ORNA_ASSERT_ALIGN(OrnaRuntimeDescriptorV1, 8);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, abi_major, 0);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, abi_minor, 4);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, runtime_name, 8);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, runtime_version, 24);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, build_id, 40);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, platform, 56);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, thread_model, 72);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, features, 80);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, sinks, 88);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, sink_count, 96);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, contracts, 104);
ORNA_ASSERT_OFFSET(OrnaRuntimeDescriptorV1, contract_count, 112);

ORNA_ASSERT_SIZE(OrnaValueRefV1, 40);
ORNA_ASSERT_ALIGN(OrnaValueRefV1, 8);
ORNA_ASSERT_OFFSET(OrnaValueRefV1, handle, 0);
ORNA_ASSERT_OFFSET(OrnaValueRefV1, type_name, 8);
ORNA_ASSERT_OFFSET(OrnaValueRefV1, canonical_encoding, 24);

ORNA_ASSERT_SIZE(OrnaUiOperationKindV1, 4);
ORNA_ASSERT_ALIGN(OrnaUiOperationKindV1, 4);
_Static_assert(ORNA_UI_OP_MOUNT_NODE == 1, "ORNA_UI_OP_MOUNT_NODE");
_Static_assert(ORNA_UI_OP_UNMOUNT_NODE == 2, "ORNA_UI_OP_UNMOUNT_NODE");
_Static_assert(ORNA_UI_OP_SET_PROPERTY == 3, "ORNA_UI_OP_SET_PROPERTY");
_Static_assert(ORNA_UI_OP_CLEAR_PROPERTY == 4, "ORNA_UI_OP_CLEAR_PROPERTY");
_Static_assert(ORNA_UI_OP_INSERT_CHILD == 5, "ORNA_UI_OP_INSERT_CHILD");
_Static_assert(ORNA_UI_OP_REMOVE_CHILD == 6, "ORNA_UI_OP_REMOVE_CHILD");
_Static_assert(ORNA_UI_OP_MOVE_CHILD == 7, "ORNA_UI_OP_MOVE_CHILD");
_Static_assert(ORNA_UI_OP_BIND_ACTION == 8, "ORNA_UI_OP_BIND_ACTION");
_Static_assert(ORNA_UI_OP_UNBIND_ACTION == 9, "ORNA_UI_OP_UNBIND_ACTION");
_Static_assert(ORNA_UI_OP_SET_FOCUS == 10, "ORNA_UI_OP_SET_FOCUS");
_Static_assert(ORNA_UI_OP_SET_ACCESSIBILITY == 11, "ORNA_UI_OP_SET_ACCESSIBILITY");

ORNA_ASSERT_SIZE(OrnaMountNodeV1, 104);
ORNA_ASSERT_ALIGN(OrnaMountNodeV1, 8);
ORNA_ASSERT_OFFSET(OrnaMountNodeV1, node, 0);
ORNA_ASSERT_OFFSET(OrnaMountNodeV1, parent, 8);
ORNA_ASSERT_OFFSET(OrnaMountNodeV1, slot, 16);
ORNA_ASSERT_OFFSET(OrnaMountNodeV1, ordinal, 32);
ORNA_ASSERT_OFFSET(OrnaMountNodeV1, contract_name, 40);
ORNA_ASSERT_OFFSET(OrnaMountNodeV1, contract_major, 56);
ORNA_ASSERT_OFFSET(OrnaMountNodeV1, contract_minor, 60);
ORNA_ASSERT_OFFSET(OrnaMountNodeV1, explicit_key, 64);

ORNA_ASSERT_SIZE(OrnaSetPropertyV1, 64);
ORNA_ASSERT_ALIGN(OrnaSetPropertyV1, 8);
ORNA_ASSERT_OFFSET(OrnaSetPropertyV1, node, 0);
ORNA_ASSERT_OFFSET(OrnaSetPropertyV1, property, 8);
ORNA_ASSERT_OFFSET(OrnaSetPropertyV1, value, 24);

ORNA_ASSERT_SIZE(OrnaChildOperationV1, 40);
ORNA_ASSERT_ALIGN(OrnaChildOperationV1, 8);
ORNA_ASSERT_OFFSET(OrnaChildOperationV1, parent, 0);
ORNA_ASSERT_OFFSET(OrnaChildOperationV1, slot, 8);
ORNA_ASSERT_OFFSET(OrnaChildOperationV1, child, 24);
ORNA_ASSERT_OFFSET(OrnaChildOperationV1, ordinal, 32);

ORNA_ASSERT_SIZE(OrnaBindActionV1, 48);
ORNA_ASSERT_ALIGN(OrnaBindActionV1, 8);
ORNA_ASSERT_OFFSET(OrnaBindActionV1, node, 0);
ORNA_ASSERT_OFFSET(OrnaBindActionV1, event_name, 8);
ORNA_ASSERT_OFFSET(OrnaBindActionV1, action, 24);
ORNA_ASSERT_OFFSET(OrnaBindActionV1, input_type, 32);

ORNA_ASSERT_MEMBER_SIZE(OrnaUiOperationV1, as, 104);
ORNA_ASSERT_SIZE(OrnaUiOperationV1, 112);
ORNA_ASSERT_ALIGN(OrnaUiOperationV1, 8);
ORNA_ASSERT_OFFSET(OrnaUiOperationV1, kind, 0);
ORNA_ASSERT_OFFSET(OrnaUiOperationV1, as, 8);

ORNA_ASSERT_SIZE(OrnaUiBatchV1, 24);
ORNA_ASSERT_ALIGN(OrnaUiBatchV1, 8);
ORNA_ASSERT_OFFSET(OrnaUiBatchV1, semantic_revision, 0);
ORNA_ASSERT_OFFSET(OrnaUiBatchV1, operations, 8);
ORNA_ASSERT_OFFSET(OrnaUiBatchV1, operation_count, 16);

ORNA_ASSERT_SIZE(OrnaRuntimeEventKindV1, 4);
ORNA_ASSERT_ALIGN(OrnaRuntimeEventKindV1, 4);
_Static_assert(ORNA_RUNTIME_EVENT_ACTION == 1, "ORNA_RUNTIME_EVENT_ACTION");
_Static_assert(ORNA_RUNTIME_EVENT_FOCUS_CHANGED == 2, "ORNA_RUNTIME_EVENT_FOCUS_CHANGED");
_Static_assert(ORNA_RUNTIME_EVENT_LAYOUT_STATE_CHANGED == 3, "ORNA_RUNTIME_EVENT_LAYOUT_STATE_CHANGED");
_Static_assert(ORNA_RUNTIME_EVENT_SURFACE_CLOSED == 4, "ORNA_RUNTIME_EVENT_SURFACE_CLOSED");
_Static_assert(ORNA_RUNTIME_EVENT_MODEL_RANGE_REQUEST == 5, "ORNA_RUNTIME_EVENT_MODEL_RANGE_REQUEST");
_Static_assert(ORNA_RUNTIME_EVENT_MODEL_CHILDREN_REQUEST == 6, "ORNA_RUNTIME_EVENT_MODEL_CHILDREN_REQUEST");
_Static_assert(ORNA_RUNTIME_EVENT_DIAGNOSTIC == 7, "ORNA_RUNTIME_EVENT_DIAGNOSTIC");

ORNA_ASSERT_SIZE(OrnaActionEventV1, 64);
ORNA_ASSERT_ALIGN(OrnaActionEventV1, 8);
ORNA_ASSERT_OFFSET(OrnaActionEventV1, surface, 0);
ORNA_ASSERT_OFFSET(OrnaActionEventV1, node, 8);
ORNA_ASSERT_OFFSET(OrnaActionEventV1, action, 16);
ORNA_ASSERT_OFFSET(OrnaActionEventV1, payload, 24);

ORNA_ASSERT_SIZE(OrnaLayoutStateEventV1, 88);
ORNA_ASSERT_ALIGN(OrnaLayoutStateEventV1, 8);
ORNA_ASSERT_OFFSET(OrnaLayoutStateEventV1, surface, 0);
ORNA_ASSERT_OFFSET(OrnaLayoutStateEventV1, node, 8);
ORNA_ASSERT_OFFSET(OrnaLayoutStateEventV1, semantic_state_name, 16);
ORNA_ASSERT_OFFSET(OrnaLayoutStateEventV1, semantic_state, 32);
ORNA_ASSERT_OFFSET(OrnaLayoutStateEventV1, opaque_runtime_state, 72);

ORNA_ASSERT_SIZE(OrnaModelRangeRequestV1, 48);
ORNA_ASSERT_ALIGN(OrnaModelRangeRequestV1, 8);
ORNA_ASSERT_OFFSET(OrnaModelRangeRequestV1, request, 0);
ORNA_ASSERT_OFFSET(OrnaModelRangeRequestV1, model, 8);
ORNA_ASSERT_OFFSET(OrnaModelRangeRequestV1, start, 16);
ORNA_ASSERT_OFFSET(OrnaModelRangeRequestV1, count, 24);
ORNA_ASSERT_OFFSET(OrnaModelRangeRequestV1, sort_filter_token, 32);

ORNA_ASSERT_SIZE(OrnaModelChildrenRequestV1, 56);
ORNA_ASSERT_ALIGN(OrnaModelChildrenRequestV1, 8);
ORNA_ASSERT_OFFSET(OrnaModelChildrenRequestV1, request, 0);
ORNA_ASSERT_OFFSET(OrnaModelChildrenRequestV1, model, 8);
ORNA_ASSERT_OFFSET(OrnaModelChildrenRequestV1, parent_key, 16);

ORNA_ASSERT_MEMBER_SIZE(OrnaRuntimeEventV1, as, 88);
ORNA_ASSERT_SIZE(OrnaRuntimeEventV1, 96);
ORNA_ASSERT_ALIGN(OrnaRuntimeEventV1, 8);
ORNA_ASSERT_OFFSET(OrnaRuntimeEventV1, kind, 0);
ORNA_ASSERT_OFFSET(OrnaRuntimeEventV1, as, 8);

ORNA_ASSERT_SIZE(OrnaClientApiV1, 72);
ORNA_ASSERT_ALIGN(OrnaClientApiV1, 8);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, abi_major, 0);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, abi_minor, 4);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, context, 8);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, log, 16);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, emit_runtime_event, 24);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, complete_model_request, 32);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, fail_model_request, 40);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, read_action_metadata, 48);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, read_value_debug_json, 56);
ORNA_ASSERT_OFFSET(OrnaClientApiV1, monotonic_time_ns, 64);

ORNA_ASSERT_SIZE(OrnaRuntimeCreateOptionsV1, 88);
ORNA_ASSERT_ALIGN(OrnaRuntimeCreateOptionsV1, 8);
ORNA_ASSERT_OFFSET(OrnaRuntimeCreateOptionsV1, client, 0);
ORNA_ASSERT_OFFSET(OrnaRuntimeCreateOptionsV1, locale, 8);
ORNA_ASSERT_OFFSET(OrnaRuntimeCreateOptionsV1, timezone, 24);
ORNA_ASSERT_OFFSET(OrnaRuntimeCreateOptionsV1, theme, 40);
ORNA_ASSERT_OFFSET(OrnaRuntimeCreateOptionsV1, accessibility_preferences_json, 56);
ORNA_ASSERT_OFFSET(OrnaRuntimeCreateOptionsV1, runtime_configuration_json, 72);

ORNA_ASSERT_SIZE(OrnaSurfaceCreateOptionsV1, 64);
ORNA_ASSERT_ALIGN(OrnaSurfaceCreateOptionsV1, 8);
ORNA_ASSERT_OFFSET(OrnaSurfaceCreateOptionsV1, surface_kind, 0);
ORNA_ASSERT_OFFSET(OrnaSurfaceCreateOptionsV1, title, 16);
ORNA_ASSERT_OFFSET(OrnaSurfaceCreateOptionsV1, state_profile, 32);
ORNA_ASSERT_OFFSET(OrnaSurfaceCreateOptionsV1, opaque_runtime_restore_state, 48);

ORNA_ASSERT_SIZE(OrnaRuntimeApiV1, 120);
ORNA_ASSERT_ALIGN(OrnaRuntimeApiV1, 8);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, abi_major, 0);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, abi_minor, 4);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, describe, 8);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, create, 16);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, destroy, 24);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, start_event_loop, 32);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, poll_event_loop, 40);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, request_shutdown, 48);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, create_surface, 56);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, destroy_surface, 64);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, apply_ui_batch, 72);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, set_surface_visible, 80);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, capture_semantic_state, 88);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, capture_opaque_state, 96);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, apply_model_rows, 104);
ORNA_ASSERT_OFFSET(OrnaRuntimeApiV1, cancel_request, 112);


/* Callback and runtime table signatures are mirrored by the Rust extern "C" fn fields. */
#define ORNA_ASSERT_TYPE(expr, expected, message) \
    _Static_assert(_Generic((expr), expected: 1, default: 0), message)

typedef void (*OrnaExpectedLogFn)(void *, uint32_t, OrnaStringView, OrnaStringView);
typedef OrnaStatus (*OrnaExpectedEmitRuntimeEventFn)(
    void *, OrnaRuntimeHandle, const OrnaRuntimeEventV1 *);
typedef OrnaStatus (*OrnaExpectedCompleteModelRequestFn)(
    void *, OrnaRequestHandle, OrnaValueRefV1);
typedef OrnaStatus (*OrnaExpectedFailModelRequestFn)(void *, OrnaRequestHandle, OrnaStatus);
typedef OrnaStatus (*OrnaExpectedReadActionMetadataFn)(
    void *, OrnaActionHandle, OrnaOwnedBytes *);
typedef OrnaStatus (*OrnaExpectedReadValueDebugJsonFn)(
    void *, OrnaValueRefV1, OrnaOwnedBytes *);
typedef uint64_t (*OrnaExpectedMonotonicTimeFn)(void *);

ORNA_ASSERT_TYPE(((OrnaClientApiV1 *)0)->log, OrnaExpectedLogFn, "ClientApi.log type");
ORNA_ASSERT_TYPE(
    ((OrnaClientApiV1 *)0)->emit_runtime_event,
    OrnaExpectedEmitRuntimeEventFn,
    "ClientApi.emit_runtime_event type");
ORNA_ASSERT_TYPE(
    ((OrnaClientApiV1 *)0)->complete_model_request,
    OrnaExpectedCompleteModelRequestFn,
    "ClientApi.complete_model_request type");
ORNA_ASSERT_TYPE(
    ((OrnaClientApiV1 *)0)->fail_model_request,
    OrnaExpectedFailModelRequestFn,
    "ClientApi.fail_model_request type");
ORNA_ASSERT_TYPE(
    ((OrnaClientApiV1 *)0)->read_action_metadata,
    OrnaExpectedReadActionMetadataFn,
    "ClientApi.read_action_metadata type");
ORNA_ASSERT_TYPE(
    ((OrnaClientApiV1 *)0)->read_value_debug_json,
    OrnaExpectedReadValueDebugJsonFn,
    "ClientApi.read_value_debug_json type");
ORNA_ASSERT_TYPE(
    ((OrnaClientApiV1 *)0)->monotonic_time_ns,
    OrnaExpectedMonotonicTimeFn,
    "ClientApi.monotonic_time_ns type");

typedef const OrnaRuntimeDescriptorV1 *(*OrnaExpectedDescribeFn)(void);
typedef OrnaStatus (*OrnaExpectedCreateFn)(
    const OrnaRuntimeCreateOptionsV1 *, OrnaRuntimeHandle *);
typedef void (*OrnaExpectedDestroyFn)(OrnaRuntimeHandle);
typedef OrnaStatus (*OrnaExpectedStartEventLoopFn)(OrnaRuntimeHandle);
typedef OrnaStatus (*OrnaExpectedPollEventLoopFn)(OrnaRuntimeHandle, uint32_t);
typedef OrnaStatus (*OrnaExpectedRequestShutdownFn)(OrnaRuntimeHandle);
typedef OrnaStatus (*OrnaExpectedCreateSurfaceFn)(
    OrnaRuntimeHandle, const OrnaSurfaceCreateOptionsV1 *, OrnaSurfaceHandle *);
typedef OrnaStatus (*OrnaExpectedDestroySurfaceFn)(OrnaRuntimeHandle, OrnaSurfaceHandle);
typedef OrnaStatus (*OrnaExpectedApplyUiBatchFn)(
    OrnaRuntimeHandle, OrnaSurfaceHandle, const OrnaUiBatchV1 *);
typedef OrnaStatus (*OrnaExpectedSetSurfaceVisibleFn)(
    OrnaRuntimeHandle, OrnaSurfaceHandle, uint8_t);
typedef OrnaStatus (*OrnaExpectedCaptureStateFn)(
    OrnaRuntimeHandle, OrnaSurfaceHandle, OrnaOwnedBytes *);
typedef OrnaStatus (*OrnaExpectedApplyModelRowsFn)(
    OrnaRuntimeHandle, OrnaRequestHandle, OrnaValueRefV1);
typedef OrnaStatus (*OrnaExpectedCancelRequestFn)(OrnaRuntimeHandle, OrnaRequestHandle);

ORNA_ASSERT_TYPE(((OrnaRuntimeApiV1 *)0)->describe, OrnaExpectedDescribeFn, "RuntimeApi.describe type");
ORNA_ASSERT_TYPE(((OrnaRuntimeApiV1 *)0)->create, OrnaExpectedCreateFn, "RuntimeApi.create type");
ORNA_ASSERT_TYPE(((OrnaRuntimeApiV1 *)0)->destroy, OrnaExpectedDestroyFn, "RuntimeApi.destroy type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->start_event_loop,
    OrnaExpectedStartEventLoopFn,
    "RuntimeApi.start_event_loop type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->poll_event_loop,
    OrnaExpectedPollEventLoopFn,
    "RuntimeApi.poll_event_loop type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->request_shutdown,
    OrnaExpectedRequestShutdownFn,
    "RuntimeApi.request_shutdown type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->create_surface,
    OrnaExpectedCreateSurfaceFn,
    "RuntimeApi.create_surface type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->destroy_surface,
    OrnaExpectedDestroySurfaceFn,
    "RuntimeApi.destroy_surface type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->apply_ui_batch,
    OrnaExpectedApplyUiBatchFn,
    "RuntimeApi.apply_ui_batch type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->set_surface_visible,
    OrnaExpectedSetSurfaceVisibleFn,
    "RuntimeApi.set_surface_visible type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->capture_semantic_state,
    OrnaExpectedCaptureStateFn,
    "RuntimeApi.capture_semantic_state type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->capture_opaque_state,
    OrnaExpectedCaptureStateFn,
    "RuntimeApi.capture_opaque_state type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->apply_model_rows,
    OrnaExpectedApplyModelRowsFn,
    "RuntimeApi.apply_model_rows type");
ORNA_ASSERT_TYPE(
    ((OrnaRuntimeApiV1 *)0)->cancel_request,
    OrnaExpectedCancelRequestFn,
    "RuntimeApi.cancel_request type");
