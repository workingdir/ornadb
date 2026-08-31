/*
 * Test-only C declaration fixture for the accepted headless runtime ABI.
 *
 * The canonical spec/spec/orna_runtime_abi_v1.h is not present in this
 * checkout. Keep this fixture limited to the declarations consumed by
 * runtime_abi_parity.c; when the canonical header is available, that test
 * includes it instead.
 */
#ifndef ORNA_RUNTIME_ABI_V1_FIXTURE_H
#define ORNA_RUNTIME_ABI_V1_FIXTURE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define ORNA_RUNTIME_ABI_V1_MAJOR 1u
#define ORNA_RUNTIME_ABI_V1_MINOR 0u

typedef uint64_t OrnaHandle;
typedef OrnaHandle OrnaRuntimeHandle;
typedef OrnaHandle OrnaSurfaceHandle;
typedef OrnaHandle OrnaNodeHandle;
typedef OrnaHandle OrnaActionHandle;
typedef OrnaHandle OrnaModelHandle;
typedef OrnaHandle OrnaRequestHandle;

typedef struct OrnaStringView {
    const char *data;
    size_t len;
} OrnaStringView;

typedef struct OrnaBytesView {
    const uint8_t *data;
    size_t len;
} OrnaBytesView;

typedef void (*OrnaReleaseFn)(void *owner, uint8_t *data, size_t len);

typedef struct OrnaOwnedBytes {
    uint8_t *data;
    size_t len;
    void *owner;
    OrnaReleaseFn release;
} OrnaOwnedBytes;

typedef enum OrnaStatusCode {
    ORNA_STATUS_OK = 0,
    ORNA_STATUS_INVALID_ARGUMENT = 1,
    ORNA_STATUS_UNSUPPORTED = 2,
    ORNA_STATUS_NOT_FOUND = 3,
    ORNA_STATUS_BUSY = 4,
    ORNA_STATUS_CANCELLED = 5,
    ORNA_STATUS_FAILED = 6,
    ORNA_STATUS_INTERNAL = 7,
    ORNA_STATUS_STALE_REVISION = 8,
} OrnaStatusCode;

typedef struct OrnaStatus {
    OrnaStatusCode code;
    OrnaStringView message;
} OrnaStatus;

typedef struct OrnaSurfaceClosedEventV1 {
    OrnaSurfaceHandle surface;
} OrnaSurfaceClosedEventV1;

typedef struct OrnaDiagnosticEventV1 {
    OrnaStatus status;
} OrnaDiagnosticEventV1;

typedef enum OrnaThreadModel {
    ORNA_THREAD_MODEL_CLIENT_EVENT_LOOP = 1,
    ORNA_THREAD_MODEL_RUNTIME_EVENT_LOOP = 2,
    ORNA_THREAD_MODEL_CALLER_PUMPS = 3,
} OrnaThreadModel;

typedef enum OrnaRuntimeFeature {
    ORNA_RUNTIME_FEATURE_MULTIPLE_WINDOWS = (1u << 0),
    ORNA_RUNTIME_FEATURE_ACCESSIBILITY = (1u << 1),
    ORNA_RUNTIME_FEATURE_CLIPBOARD = (1u << 2),
    ORNA_RUNTIME_FEATURE_DRAG_DROP = (1u << 3),
    ORNA_RUNTIME_FEATURE_NATIVE_MENUS = (1u << 4),
    ORNA_RUNTIME_FEATURE_PRINTING = (1u << 5),
    ORNA_RUNTIME_FEATURE_OPAQUE_LAYOUT_STATE = (1u << 6),
} OrnaRuntimeFeature;

typedef struct OrnaContractVersionV1 {
    OrnaStringView name;
    uint32_t major;
    uint32_t minor;
    const OrnaStringView *features;
    size_t feature_count;
} OrnaContractVersionV1;

typedef struct OrnaSinkOfferV1 {
    OrnaStringView type_name;
    const OrnaStringView *media_types;
    size_t media_type_count;
    uint8_t supports_streaming;
    int32_t preference_rank;
} OrnaSinkOfferV1;

typedef struct OrnaRuntimeDescriptorV1 {
    uint32_t abi_major;
    uint32_t abi_minor;
    OrnaStringView runtime_name;
    OrnaStringView runtime_version;
    OrnaStringView build_id;
    OrnaStringView platform;
    OrnaThreadModel thread_model;
    uint64_t features;
    const OrnaSinkOfferV1 *sinks;
    size_t sink_count;
    const OrnaContractVersionV1 *contracts;
    size_t contract_count;
} OrnaRuntimeDescriptorV1;

typedef struct OrnaValueRefV1 {
    OrnaHandle handle;
    OrnaStringView type_name;
    OrnaBytesView canonical_encoding;
} OrnaValueRefV1;

typedef enum OrnaUiOperationKindV1 {
    ORNA_UI_OP_MOUNT_NODE = 1,
    ORNA_UI_OP_UNMOUNT_NODE = 2,
    ORNA_UI_OP_SET_PROPERTY = 3,
    ORNA_UI_OP_CLEAR_PROPERTY = 4,
    ORNA_UI_OP_INSERT_CHILD = 5,
    ORNA_UI_OP_REMOVE_CHILD = 6,
    ORNA_UI_OP_MOVE_CHILD = 7,
    ORNA_UI_OP_BIND_ACTION = 8,
    ORNA_UI_OP_UNBIND_ACTION = 9,
    ORNA_UI_OP_SET_FOCUS = 10,
    ORNA_UI_OP_SET_ACCESSIBILITY = 11,
} OrnaUiOperationKindV1;

typedef struct OrnaMountNodeV1 {
    OrnaNodeHandle node;
    OrnaNodeHandle parent;
    OrnaStringView slot;
    size_t ordinal;
    OrnaStringView contract_name;
    uint32_t contract_major;
    uint32_t contract_minor;
    OrnaValueRefV1 explicit_key;
} OrnaMountNodeV1;

typedef struct OrnaSetPropertyV1 {
    OrnaNodeHandle node;
    OrnaStringView property;
    OrnaValueRefV1 value;
} OrnaSetPropertyV1;

typedef struct OrnaChildOperationV1 {
    OrnaNodeHandle parent;
    OrnaStringView slot;
    OrnaNodeHandle child;
    size_t ordinal;
} OrnaChildOperationV1;

typedef struct OrnaBindActionV1 {
    OrnaNodeHandle node;
    OrnaStringView event_name;
    OrnaActionHandle action;
    OrnaStringView input_type;
} OrnaBindActionV1;

typedef union OrnaUiOperationArgsV1 {
    OrnaMountNodeV1 mount_node;
    OrnaNodeHandle unmount_node;
    OrnaSetPropertyV1 set_property;
    OrnaChildOperationV1 child;
    OrnaBindActionV1 bind_action;
} OrnaUiOperationArgsV1;

typedef struct OrnaUiOperationV1 {
    OrnaUiOperationKindV1 kind;
    OrnaUiOperationArgsV1 as;
} OrnaUiOperationV1;

typedef struct OrnaUiBatchV1 {
    uint64_t semantic_revision;
    const OrnaUiOperationV1 *operations;
    size_t operation_count;
} OrnaUiBatchV1;

typedef enum OrnaRuntimeEventKindV1 {
    ORNA_RUNTIME_EVENT_ACTION = 1,
    ORNA_RUNTIME_EVENT_FOCUS_CHANGED = 2,
    ORNA_RUNTIME_EVENT_LAYOUT_STATE_CHANGED = 3,
    ORNA_RUNTIME_EVENT_SURFACE_CLOSED = 4,
    ORNA_RUNTIME_EVENT_MODEL_RANGE_REQUEST = 5,
    ORNA_RUNTIME_EVENT_MODEL_CHILDREN_REQUEST = 6,
    ORNA_RUNTIME_EVENT_DIAGNOSTIC = 7,
} OrnaRuntimeEventKindV1;

typedef struct OrnaActionEventV1 {
    OrnaSurfaceHandle surface;
    OrnaNodeHandle node;
    OrnaActionHandle action;
    OrnaValueRefV1 payload;
} OrnaActionEventV1;

typedef struct OrnaLayoutStateEventV1 {
    OrnaSurfaceHandle surface;
    OrnaNodeHandle node;
    OrnaStringView semantic_state_name;
    OrnaValueRefV1 semantic_state;
    OrnaBytesView opaque_runtime_state;
} OrnaLayoutStateEventV1;

typedef struct OrnaModelRangeRequestV1 {
    OrnaRequestHandle request;
    OrnaModelHandle model;
    uint64_t start;
    uint64_t count;
    OrnaStringView sort_filter_token;
} OrnaModelRangeRequestV1;

typedef struct OrnaModelChildrenRequestV1 {
    OrnaRequestHandle request;
    OrnaModelHandle model;
    OrnaValueRefV1 parent_key;
} OrnaModelChildrenRequestV1;

typedef union OrnaRuntimeEventArgsV1 {
    OrnaActionEventV1 action;
    OrnaLayoutStateEventV1 layout_state;
    OrnaModelRangeRequestV1 range_request;
    OrnaModelChildrenRequestV1 children_request;
    OrnaSurfaceClosedEventV1 surface_closed;
    OrnaDiagnosticEventV1 diagnostic;
} OrnaRuntimeEventArgsV1;

typedef struct OrnaRuntimeEventV1 {
    OrnaRuntimeEventKindV1 kind;
    OrnaRuntimeEventArgsV1 as;
} OrnaRuntimeEventV1;

typedef void (*OrnaLogFn)(void *context,
                          uint32_t level,
                          OrnaStringView subsystem,
                          OrnaStringView message);
typedef OrnaStatus (*OrnaEmitRuntimeEventFn)(void *context,
                                            OrnaRuntimeHandle runtime,
                                            const OrnaRuntimeEventV1 *event);
typedef OrnaStatus (*OrnaCompleteModelRequestFn)(void *context,
                                                OrnaRequestHandle request,
                                                OrnaValueRefV1 result);
typedef OrnaStatus (*OrnaFailModelRequestFn)(void *context,
                                             OrnaRequestHandle request,
                                             OrnaStatus failure);
typedef OrnaStatus (*OrnaReadActionMetadataFn)(void *context,
                                              OrnaActionHandle action,
                                              OrnaOwnedBytes *out_metadata);
typedef OrnaStatus (*OrnaReadValueDebugJsonFn)(void *context,
                                               OrnaValueRefV1 value,
                                               OrnaOwnedBytes *out_json);
typedef uint64_t (*OrnaMonotonicTimeFn)(void *context);

typedef struct OrnaClientApiV1 {
    uint32_t abi_major;
    uint32_t abi_minor;
    void *context;
    OrnaLogFn log;
    OrnaEmitRuntimeEventFn emit_runtime_event;
    OrnaCompleteModelRequestFn complete_model_request;
    OrnaFailModelRequestFn fail_model_request;
    OrnaReadActionMetadataFn read_action_metadata;
    OrnaReadValueDebugJsonFn read_value_debug_json;
    OrnaMonotonicTimeFn monotonic_time_ns;
} OrnaClientApiV1;

typedef struct OrnaRuntimeCreateOptionsV1 {
    const OrnaClientApiV1 *client;
    OrnaStringView locale;
    OrnaStringView timezone;
    OrnaStringView theme;
    OrnaStringView accessibility_preferences_json;
    OrnaStringView runtime_configuration_json;
} OrnaRuntimeCreateOptionsV1;

typedef struct OrnaSurfaceCreateOptionsV1 {
    OrnaStringView surface_kind;
    OrnaStringView title;
    OrnaStringView state_profile;
    OrnaBytesView opaque_runtime_restore_state;
} OrnaSurfaceCreateOptionsV1;

typedef const OrnaRuntimeDescriptorV1 *(*OrnaDescribeFn)(void);
typedef OrnaStatus (*OrnaCreateFn)(const OrnaRuntimeCreateOptionsV1 *options,
                                   OrnaRuntimeHandle *out_runtime);
typedef void (*OrnaDestroyFn)(OrnaRuntimeHandle runtime);
typedef OrnaStatus (*OrnaStartEventLoopFn)(OrnaRuntimeHandle runtime);
typedef OrnaStatus (*OrnaPollEventLoopFn)(OrnaRuntimeHandle runtime, uint32_t timeout_ms);
typedef OrnaStatus (*OrnaRequestShutdownFn)(OrnaRuntimeHandle runtime);
typedef OrnaStatus (*OrnaCreateSurfaceFn)(OrnaRuntimeHandle runtime,
                                          const OrnaSurfaceCreateOptionsV1 *options,
                                          OrnaSurfaceHandle *out_surface);
typedef OrnaStatus (*OrnaDestroySurfaceFn)(OrnaRuntimeHandle runtime,
                                          OrnaSurfaceHandle surface);
typedef OrnaStatus (*OrnaApplyUiBatchFn)(OrnaRuntimeHandle runtime,
                                         OrnaSurfaceHandle surface,
                                         const OrnaUiBatchV1 *batch);
typedef OrnaStatus (*OrnaSetSurfaceVisibleFn)(OrnaRuntimeHandle runtime,
                                              OrnaSurfaceHandle surface,
                                              uint8_t visible);
typedef OrnaStatus (*OrnaCaptureStateFn)(OrnaRuntimeHandle runtime,
                                         OrnaSurfaceHandle surface,
                                         OrnaOwnedBytes *out_state);
typedef OrnaStatus (*OrnaApplyModelRowsFn)(OrnaRuntimeHandle runtime,
                                           OrnaRequestHandle request,
                                           OrnaValueRefV1 rows);
typedef OrnaStatus (*OrnaCancelRequestFn)(OrnaRuntimeHandle runtime,
                                          OrnaRequestHandle request);

typedef struct OrnaRuntimeApiV1 {
    uint32_t abi_major;
    uint32_t abi_minor;
    OrnaDescribeFn describe;
    OrnaCreateFn create;
    OrnaDestroyFn destroy;
    OrnaStartEventLoopFn start_event_loop;
    OrnaPollEventLoopFn poll_event_loop;
    OrnaRequestShutdownFn request_shutdown;
    OrnaCreateSurfaceFn create_surface;
    OrnaDestroySurfaceFn destroy_surface;
    OrnaApplyUiBatchFn apply_ui_batch;
    OrnaSetSurfaceVisibleFn set_surface_visible;
    OrnaCaptureStateFn capture_semantic_state;
    OrnaCaptureStateFn capture_opaque_state;
    OrnaApplyModelRowsFn apply_model_rows;
    OrnaCancelRequestFn cancel_request;
} OrnaRuntimeApiV1;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* ORNA_RUNTIME_ABI_V1_FIXTURE_H */
