//! Trusted loading and descriptor validation for the production Qt runtime.
//!
//! The ABI declarations in this module intentionally mirror
//! `spec/spec/orna_runtime_abi_v1.h`.  The raw declarations are only used at
//! the dynamic-library boundary.  [`RuntimeLibrary`] copies the API table and
//! descriptor into Rust-owned values while retaining the [`libloading::Library`]
//! that keeps every copied function pointer valid.

use std::{
    collections::HashSet,
    ffi::{c_char, c_void},
    fmt,
    path::Path,
    slice, str,
};

use libloading::Library;

/// ABI v1 major version accepted by this client.
pub const ABI_V1_MAJOR: u32 = 1;
/// ABI v1 minor version accepted by this client.
pub const ABI_V1_MINOR: u32 = 0;

/// The exact runtime family accepted by [`RuntimeLibrary::load_qt`].
pub const QT_RUNTIME_NAME: &str = "orna-runtime-qt";
/// The exact target platform accepted by [`RuntimeLibrary::load_qt`].
pub const QT_PLATFORM: &str = "linux-x86_64";
/// The sink consumed by the first Qt provider.
pub const UI_SINK_NAME: &str = "std.ui.UI";

/// Maximum bytes read from one descriptor string.
///
/// Descriptor identity and offer names are metadata, not runtime values.  The
/// bound is deliberately much smaller than the 16 MiB value payload bound so
/// a malformed descriptor cannot force an unbounded allocation.
pub const CLIENT_MAX_DESCRIPTOR_STRING_BYTES: usize = 4096;
/// Maximum number of media types in one sink offer.
pub const CLIENT_MAX_SINK_MEDIA_TYPES: usize = 16;
/// Maximum number of feature names in one contract offer.
pub const CLIENT_MAX_CONTRACT_FEATURES: usize = 16;
/// Maximum number of sink offers accepted from the Qt provider.
pub const CLIENT_MAX_SINK_OFFERS: usize = 1;
/// Maximum number of structural contract offers accepted from the Qt provider.
pub const CLIENT_MAX_CONTRACT_OFFERS: usize = 8;

/// The Qt provider advertises support for multiple windows and no other v1
/// runtime feature yet.
pub const RUNTIME_FEATURE_MULTIPLE_WINDOWS: u64 = 1 << 0;

const QUERY_SYMBOL: &[u8] = b"orna_runtime_query_v1\0";
const REQUIRED_CONTRACTS: [(&str, u32, u32); CLIENT_MAX_CONTRACT_OFFERS] = [
    ("std.ui.window", 1, 0),
    ("std.ui.text", 1, 0),
    ("std.ui.button", 1, 0),
    ("std.ui.panel", 1, 0),
    ("std.ui.row", 1, 0),
    ("std.ui.column", 1, 0),
    ("std.ui.text_input", 1, 0),
    ("std.ui.tabs", 1, 0),
];

// ---------------------------------------------------------------------------
// C ABI mirror
// ---------------------------------------------------------------------------

/// `OrnaHandle` and all v1 handle aliases have the same representation.
pub type AbiHandle = u64;
/// `OrnaRuntimeHandle`.
pub type AbiRuntimeHandle = AbiHandle;
/// `OrnaSurfaceHandle`.
pub type AbiSurfaceHandle = AbiHandle;
/// `OrnaNodeHandle`.
pub type AbiNodeHandle = AbiHandle;
/// `OrnaActionHandle`.
pub type AbiActionHandle = AbiHandle;
/// `OrnaModelHandle`.
pub type AbiModelHandle = AbiHandle;
/// `OrnaRequestHandle`.
pub type AbiRequestHandle = AbiHandle;

/// C `OrnaStringView`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiStringView {
    pub data: *const c_char,
    pub len: usize,
}

/// C `OrnaBytesView`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiBytesView {
    pub data: *const u8,
    pub len: usize,
}

/// C `OrnaOwnedBytes`.
#[repr(C)]
#[derive(Debug)]
pub struct AbiOwnedBytes {
    pub data: *mut u8,
    pub len: usize,
    pub owner: *mut c_void,
    pub release: ReleaseFn,
}

/// Callback that releases runtime-owned bytes.
pub type ReleaseFn = unsafe extern "C" fn(owner: *mut c_void, data: *mut u8, len: usize);

/// C `OrnaStatusCode`.
///
/// C enum values have the ABI's four-byte enum representation on the accepted
/// Linux x86_64 boundary.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbiStatusCode(pub i32);

impl AbiStatusCode {
    pub const OK: Self = Self(0);
    pub const INVALID_ARGUMENT: Self = Self(1);
    pub const UNSUPPORTED: Self = Self(2);
    pub const NOT_FOUND: Self = Self(3);
    pub const BUSY: Self = Self(4);
    pub const CANCELLED: Self = Self(5);
    pub const FAILED: Self = Self(6);
    pub const INTERNAL: Self = Self(7);
    pub const STALE_REVISION: Self = Self(8);
}

/// C `OrnaStatus`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiStatus {
    pub code: AbiStatusCode,
    pub message: AbiStringView,
}

/// C `OrnaSurfaceClosedEventV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiSurfaceClosedEvent {
    pub surface: AbiSurfaceHandle,
}

/// C `OrnaDiagnosticEventV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiDiagnosticEvent {
    pub status: AbiStatus,
}

/// C `OrnaRuntimeFeature` values.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbiRuntimeFeature(pub i32);

impl AbiRuntimeFeature {
    pub const MULTIPLE_WINDOWS: Self = Self(1 << 0);
    pub const ACCESSIBILITY: Self = Self(1 << 1);
    pub const CLIPBOARD: Self = Self(1 << 2);
    pub const DRAG_DROP: Self = Self(1 << 3);
    pub const NATIVE_MENUS: Self = Self(1 << 4);
    pub const PRINTING: Self = Self(1 << 5);
    pub const OPAQUE_LAYOUT_STATE: Self = Self(1 << 6);
}

/// C `OrnaThreadModel`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbiThreadModel(pub i32);
impl AbiThreadModel {
    pub const CLIENT_EVENT_LOOP: Self = Self(1);
    pub const RUNTIME_EVENT_LOOP: Self = Self(2);
    pub const CALLER_PUMPS: Self = Self(3);
}

/// C `OrnaContractVersionV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiContractVersion {
    pub name: AbiStringView,
    pub major: u32,
    pub minor: u32,
    pub features: *const AbiStringView,
    pub feature_count: usize,
}

/// C `OrnaSinkOfferV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiSinkOffer {
    pub type_name: AbiStringView,
    pub media_types: *const AbiStringView,
    pub media_type_count: usize,
    pub supports_streaming: u8,
    pub preference_rank: i32,
}

/// C `OrnaRuntimeDescriptorV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiDescriptor {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub runtime_name: AbiStringView,
    pub runtime_version: AbiStringView,
    pub build_id: AbiStringView,
    pub platform: AbiStringView,
    pub thread_model: AbiThreadModel,
    pub features: u64,
    pub sinks: *const AbiSinkOffer,
    pub sink_count: usize,
    pub contracts: *const AbiContractVersion,
    pub contract_count: usize,
}

/// C `OrnaValueRefV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiValueRef {
    pub handle: AbiHandle,
    pub type_name: AbiStringView,
    pub canonical_encoding: AbiBytesView,
}

/// C `OrnaUiOperationKindV1`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbiUiOperationKind(pub i32);

impl AbiUiOperationKind {
    pub const MOUNT_NODE: Self = Self(1);
    pub const UNMOUNT_NODE: Self = Self(2);
    pub const SET_PROPERTY: Self = Self(3);
    pub const CLEAR_PROPERTY: Self = Self(4);
    pub const INSERT_CHILD: Self = Self(5);
    pub const REMOVE_CHILD: Self = Self(6);
    pub const MOVE_CHILD: Self = Self(7);
    pub const BIND_ACTION: Self = Self(8);
    pub const UNBIND_ACTION: Self = Self(9);
    pub const SET_FOCUS: Self = Self(10);
    pub const SET_ACCESSIBILITY: Self = Self(11);
}

/// C `OrnaMountNodeV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiMountNode {
    pub node: AbiNodeHandle,
    pub parent: AbiNodeHandle,
    pub slot: AbiStringView,
    pub ordinal: usize,
    pub contract_name: AbiStringView,
    pub contract_major: u32,
    pub contract_minor: u32,
    pub explicit_key: AbiValueRef,
}

/// C `OrnaSetPropertyV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiSetProperty {
    pub node: AbiNodeHandle,
    pub property: AbiStringView,
    pub value: AbiValueRef,
}

/// C `OrnaChildOperationV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiChildOperation {
    pub parent: AbiNodeHandle,
    pub slot: AbiStringView,
    pub child: AbiNodeHandle,
    pub ordinal: usize,
}

/// C `OrnaBindActionV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiBindAction {
    pub node: AbiNodeHandle,
    pub event_name: AbiStringView,
    pub action: AbiActionHandle,
    pub input_type: AbiStringView,
}

/// C union for `OrnaUiOperationV1`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union AbiUiOperationArgs {
    pub mount_node: AbiMountNode,
    pub unmount_node: AbiNodeHandle,
    pub set_property: AbiSetProperty,
    pub child: AbiChildOperation,
    pub bind_action: AbiBindAction,
}

/// C `OrnaUiOperationV1`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiUiOperation {
    pub kind: AbiUiOperationKind,
    pub as_: AbiUiOperationArgs,
}

/// C `OrnaUiBatchV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiUiBatch {
    pub semantic_revision: u64,
    pub operations: *const AbiUiOperation,
    pub operation_count: usize,
}

/// C `OrnaRuntimeEventKindV1`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbiEventKind(pub i32);

impl AbiEventKind {
    pub const ACTION: Self = Self(1);
    pub const FOCUS_CHANGED: Self = Self(2);
    pub const LAYOUT_STATE_CHANGED: Self = Self(3);
    pub const SURFACE_CLOSED: Self = Self(4);
    pub const MODEL_RANGE_REQUEST: Self = Self(5);
    pub const MODEL_CHILDREN_REQUEST: Self = Self(6);
    pub const DIAGNOSTIC: Self = Self(7);
}

/// C `OrnaActionEventV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiActionEvent {
    pub surface: AbiSurfaceHandle,
    pub node: AbiNodeHandle,
    pub action: AbiActionHandle,
    pub payload: AbiValueRef,
}

/// C `OrnaLayoutStateEventV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiLayoutStateEvent {
    pub surface: AbiSurfaceHandle,
    pub node: AbiNodeHandle,
    pub semantic_state_name: AbiStringView,
    pub semantic_state: AbiValueRef,
    pub opaque_runtime_state: AbiBytesView,
}

/// C `OrnaModelRangeRequestV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiModelRangeRequest {
    pub request: AbiRequestHandle,
    pub model: AbiModelHandle,
    pub start: u64,
    pub count: u64,
    pub sort_filter_token: AbiStringView,
}

/// C `OrnaModelChildrenRequestV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiModelChildrenRequest {
    pub request: AbiRequestHandle,
    pub model: AbiModelHandle,
    pub parent_key: AbiValueRef,
}

/// C union for `OrnaRuntimeEventV1`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union AbiRuntimeEventArgs {
    pub action: AbiActionEvent,
    pub layout_state: AbiLayoutStateEvent,
    pub range_request: AbiModelRangeRequest,
    pub children_request: AbiModelChildrenRequest,
    pub surface_closed: AbiSurfaceClosedEvent,
    pub diagnostic: AbiDiagnosticEvent,
}

/// C `OrnaRuntimeEventV1`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiRuntimeEvent {
    pub kind: AbiEventKind,
    pub as_: AbiRuntimeEventArgs,
}

/// Client callback for runtime logging.
pub type LogFn = unsafe extern "C" fn(
    context: *mut c_void,
    level: u32,
    subsystem: AbiStringView,
    message: AbiStringView,
);
/// Client callback for typed runtime events.
pub type EmitRuntimeEventFn = unsafe extern "C" fn(
    context: *mut c_void,
    runtime: AbiRuntimeHandle,
    event: *const AbiRuntimeEvent,
) -> AbiStatus;
/// Client callback for model request completion.
pub type CompleteModelRequestFn = unsafe extern "C" fn(
    context: *mut c_void,
    request: AbiRequestHandle,
    result: AbiValueRef,
) -> AbiStatus;
/// Client callback for model request failure.
pub type FailModelRequestFn = unsafe extern "C" fn(
    context: *mut c_void,
    request: AbiRequestHandle,
    failure: AbiStatus,
) -> AbiStatus;
/// Client callback for action metadata.
pub type ReadActionMetadataFn = unsafe extern "C" fn(
    context: *mut c_void,
    action: AbiActionHandle,
    out_metadata: *mut AbiOwnedBytes,
) -> AbiStatus;
/// Client callback for debug JSON for one value.
pub type ReadValueDebugJsonFn = unsafe extern "C" fn(
    context: *mut c_void,
    value: AbiValueRef,
    out_json: *mut AbiOwnedBytes,
) -> AbiStatus;
/// Client callback for a monotonic clock.
pub type MonotonicTimeFn = unsafe extern "C" fn(context: *mut c_void) -> u64;

/// C `OrnaClientApiV1`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiClientApi {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub context: *mut c_void,
    pub log: LogFn,
    pub emit_runtime_event: EmitRuntimeEventFn,
    pub complete_model_request: CompleteModelRequestFn,
    pub fail_model_request: FailModelRequestFn,
    pub read_action_metadata: ReadActionMetadataFn,
    pub read_value_debug_json: ReadValueDebugJsonFn,
    pub monotonic_time_ns: MonotonicTimeFn,
}

/// C `OrnaRuntimeCreateOptionsV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiRuntimeCreateOptions {
    pub client: *const AbiClientApi,
    pub locale: AbiStringView,
    pub timezone: AbiStringView,
    pub theme: AbiStringView,
    pub accessibility_preferences_json: AbiStringView,
    pub runtime_configuration_json: AbiStringView,
}

/// C `OrnaSurfaceCreateOptionsV1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiSurfaceCreateOptions {
    pub surface_kind: AbiStringView,
    pub title: AbiStringView,
    pub state_profile: AbiStringView,
    pub opaque_runtime_restore_state: AbiBytesView,
}

/// Runtime descriptor query function exported by every v1 runtime.
pub type DescribeFn = unsafe extern "C" fn() -> *const AbiDescriptor;
/// Runtime construction function.
pub type CreateFn = unsafe extern "C" fn(
    options: *const AbiRuntimeCreateOptions,
    out_runtime: *mut AbiRuntimeHandle,
) -> AbiStatus;
/// Runtime destruction function.
pub type DestroyFn = unsafe extern "C" fn(runtime: AbiRuntimeHandle);
/// Runtime event-loop starter.
pub type StartEventLoopFn = unsafe extern "C" fn(runtime: AbiRuntimeHandle) -> AbiStatus;
/// Runtime caller-pumps event-loop poller.
pub type PollEventLoopFn =
    unsafe extern "C" fn(runtime: AbiRuntimeHandle, timeout_ms: u32) -> AbiStatus;
/// Runtime shutdown request.
pub type RequestShutdownFn = unsafe extern "C" fn(runtime: AbiRuntimeHandle) -> AbiStatus;
/// Surface creation function.
pub type CreateSurfaceFn = unsafe extern "C" fn(
    runtime: AbiRuntimeHandle,
    options: *const AbiSurfaceCreateOptions,
    out_surface: *mut AbiSurfaceHandle,
) -> AbiStatus;
/// Surface destruction function.
pub type DestroySurfaceFn =
    unsafe extern "C" fn(runtime: AbiRuntimeHandle, surface: AbiSurfaceHandle) -> AbiStatus;
/// UI batch application function.
pub type ApplyUiBatchFn = unsafe extern "C" fn(
    runtime: AbiRuntimeHandle,
    surface: AbiSurfaceHandle,
    batch: *const AbiUiBatch,
) -> AbiStatus;
/// Surface visibility function.
pub type SetSurfaceVisibleFn = unsafe extern "C" fn(
    runtime: AbiRuntimeHandle,
    surface: AbiSurfaceHandle,
    visible: u8,
) -> AbiStatus;
/// Semantic state capture function.
pub type CaptureSemanticStateFn = unsafe extern "C" fn(
    runtime: AbiRuntimeHandle,
    surface: AbiSurfaceHandle,
    out_canonical_state: *mut AbiOwnedBytes,
) -> AbiStatus;
/// Opaque state capture function.
pub type CaptureOpaqueStateFn = unsafe extern "C" fn(
    runtime: AbiRuntimeHandle,
    surface: AbiSurfaceHandle,
    out_runtime_state: *mut AbiOwnedBytes,
) -> AbiStatus;
/// Model row application function.
pub type ApplyModelRowsFn = unsafe extern "C" fn(
    runtime: AbiRuntimeHandle,
    request: AbiRequestHandle,
    rows: AbiValueRef,
) -> AbiStatus;
/// Model request cancellation function.
pub type CancelRequestFn =
    unsafe extern "C" fn(runtime: AbiRuntimeHandle, request: AbiRequestHandle) -> AbiStatus;

/// C `OrnaRuntimeApiV1`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RuntimeApi {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub describe: DescribeFn,
    pub create: CreateFn,
    pub destroy: DestroyFn,
    pub start_event_loop: StartEventLoopFn,
    pub poll_event_loop: PollEventLoopFn,
    pub request_shutdown: RequestShutdownFn,
    pub create_surface: CreateSurfaceFn,
    pub destroy_surface: DestroySurfaceFn,
    pub apply_ui_batch: ApplyUiBatchFn,
    pub set_surface_visible: SetSurfaceVisibleFn,
    pub capture_semantic_state: CaptureSemanticStateFn,
    pub capture_opaque_state: CaptureOpaqueStateFn,
    pub apply_model_rows: ApplyModelRowsFn,
    pub cancel_request: CancelRequestFn,
}

/// Query function exported by the runtime shared library.
pub type QueryFn = unsafe extern "C" fn() -> *const RuntimeApi;

// Descriptive aliases make it clear which declarations are ABI-facing when a
// caller has both an owned descriptor and a raw descriptor in scope.
pub type AbiRuntimeApi = RuntimeApi;

// The canonical header uses an ABI-sized pointer/array layout.  Keep these
// assertions next to the production mirror so accidental field changes fail at
// compile time on the supported 64-bit boundary.
#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::{align_of, offset_of, size_of};

    assert!(size_of::<AbiStringView>() == 16);
    assert!(align_of::<AbiStringView>() == 8);
    assert!(offset_of!(AbiStringView, data) == 0);
    assert!(offset_of!(AbiStringView, len) == 8);
    assert!(size_of::<AbiBytesView>() == 16);
    assert!(align_of::<AbiBytesView>() == 8);
    assert!(offset_of!(AbiBytesView, data) == 0);
    assert!(offset_of!(AbiBytesView, len) == 8);
    assert!(size_of::<AbiOwnedBytes>() == 32);
    assert!(align_of::<AbiOwnedBytes>() == 8);
    assert!(offset_of!(AbiOwnedBytes, data) == 0);
    assert!(offset_of!(AbiOwnedBytes, len) == 8);
    assert!(offset_of!(AbiOwnedBytes, owner) == 16);
    assert!(offset_of!(AbiOwnedBytes, release) == 24);
    assert!(size_of::<AbiStatusCode>() == 4);
    assert!(align_of::<AbiStatusCode>() == 4);
    assert!(size_of::<AbiStatus>() == 24);
    assert!(align_of::<AbiStatus>() == 8);
    assert!(offset_of!(AbiStatus, code) == 0);
    assert!(offset_of!(AbiStatus, message) == 8);
    assert!(size_of::<AbiSurfaceClosedEvent>() == 8);
    assert!(align_of::<AbiSurfaceClosedEvent>() == 8);
    assert!(size_of::<AbiDiagnosticEvent>() == 24);
    assert!(align_of::<AbiDiagnosticEvent>() == 8);
    assert!(size_of::<AbiRuntimeFeature>() == 4);
    assert!(align_of::<AbiRuntimeFeature>() == 4);
    assert!(size_of::<AbiThreadModel>() == 4);
    assert!(align_of::<AbiThreadModel>() == 4);
    assert!(size_of::<AbiContractVersion>() == 40);
    assert!(align_of::<AbiContractVersion>() == 8);
    assert!(offset_of!(AbiContractVersion, name) == 0);
    assert!(offset_of!(AbiContractVersion, major) == 16);
    assert!(offset_of!(AbiContractVersion, minor) == 20);
    assert!(offset_of!(AbiContractVersion, features) == 24);
    assert!(offset_of!(AbiContractVersion, feature_count) == 32);
    assert!(size_of::<AbiSinkOffer>() == 40);
    assert!(align_of::<AbiSinkOffer>() == 8);
    assert!(offset_of!(AbiSinkOffer, type_name) == 0);
    assert!(offset_of!(AbiSinkOffer, media_types) == 16);
    assert!(offset_of!(AbiSinkOffer, media_type_count) == 24);
    assert!(offset_of!(AbiSinkOffer, supports_streaming) == 32);
    assert!(offset_of!(AbiSinkOffer, preference_rank) == 36);
    assert!(size_of::<AbiDescriptor>() == 120);
    assert!(align_of::<AbiDescriptor>() == 8);
    assert!(offset_of!(AbiDescriptor, abi_major) == 0);
    assert!(offset_of!(AbiDescriptor, abi_minor) == 4);
    assert!(offset_of!(AbiDescriptor, runtime_name) == 8);
    assert!(offset_of!(AbiDescriptor, runtime_version) == 24);
    assert!(offset_of!(AbiDescriptor, build_id) == 40);
    assert!(offset_of!(AbiDescriptor, platform) == 56);
    assert!(offset_of!(AbiDescriptor, thread_model) == 72);
    assert!(offset_of!(AbiDescriptor, features) == 80);
    assert!(offset_of!(AbiDescriptor, sinks) == 88);
    assert!(offset_of!(AbiDescriptor, sink_count) == 96);
    assert!(offset_of!(AbiDescriptor, contracts) == 104);
    assert!(offset_of!(AbiDescriptor, contract_count) == 112);
    assert!(size_of::<AbiValueRef>() == 40);
    assert!(align_of::<AbiValueRef>() == 8);
    assert!(offset_of!(AbiValueRef, handle) == 0);
    assert!(offset_of!(AbiValueRef, type_name) == 8);
    assert!(offset_of!(AbiValueRef, canonical_encoding) == 24);
    assert!(size_of::<AbiUiOperationKind>() == 4);
    assert!(align_of::<AbiUiOperationKind>() == 4);
    assert!(size_of::<AbiMountNode>() == 104);
    assert!(align_of::<AbiMountNode>() == 8);
    assert!(offset_of!(AbiMountNode, node) == 0);
    assert!(offset_of!(AbiMountNode, parent) == 8);
    assert!(offset_of!(AbiMountNode, slot) == 16);
    assert!(offset_of!(AbiMountNode, ordinal) == 32);
    assert!(offset_of!(AbiMountNode, contract_name) == 40);
    assert!(offset_of!(AbiMountNode, contract_major) == 56);
    assert!(offset_of!(AbiMountNode, contract_minor) == 60);
    assert!(offset_of!(AbiMountNode, explicit_key) == 64);
    assert!(size_of::<AbiSetProperty>() == 64);
    assert!(align_of::<AbiSetProperty>() == 8);
    assert!(offset_of!(AbiSetProperty, node) == 0);
    assert!(offset_of!(AbiSetProperty, property) == 8);
    assert!(offset_of!(AbiSetProperty, value) == 24);
    assert!(size_of::<AbiChildOperation>() == 40);
    assert!(align_of::<AbiChildOperation>() == 8);
    assert!(offset_of!(AbiChildOperation, parent) == 0);
    assert!(offset_of!(AbiChildOperation, slot) == 8);
    assert!(offset_of!(AbiChildOperation, child) == 24);
    assert!(offset_of!(AbiChildOperation, ordinal) == 32);
    assert!(size_of::<AbiBindAction>() == 48);
    assert!(align_of::<AbiBindAction>() == 8);
    assert!(offset_of!(AbiBindAction, node) == 0);
    assert!(offset_of!(AbiBindAction, event_name) == 8);
    assert!(offset_of!(AbiBindAction, action) == 24);
    assert!(offset_of!(AbiBindAction, input_type) == 32);
    assert!(size_of::<AbiUiOperationArgs>() == 104);
    assert!(align_of::<AbiUiOperationArgs>() == 8);
    assert!(size_of::<AbiUiOperation>() == 112);
    assert!(align_of::<AbiUiOperation>() == 8);
    assert!(offset_of!(AbiUiOperation, kind) == 0);
    assert!(offset_of!(AbiUiOperation, as_) == 8);
    assert!(size_of::<AbiUiBatch>() == 24);
    assert!(align_of::<AbiUiBatch>() == 8);
    assert!(offset_of!(AbiUiBatch, semantic_revision) == 0);
    assert!(offset_of!(AbiUiBatch, operations) == 8);
    assert!(offset_of!(AbiUiBatch, operation_count) == 16);
    assert!(size_of::<AbiEventKind>() == 4);
    assert!(align_of::<AbiEventKind>() == 4);
    assert!(size_of::<AbiActionEvent>() == 64);
    assert!(align_of::<AbiActionEvent>() == 8);
    assert!(offset_of!(AbiActionEvent, surface) == 0);
    assert!(offset_of!(AbiActionEvent, node) == 8);
    assert!(offset_of!(AbiActionEvent, action) == 16);
    assert!(offset_of!(AbiActionEvent, payload) == 24);
    assert!(size_of::<AbiLayoutStateEvent>() == 88);
    assert!(align_of::<AbiLayoutStateEvent>() == 8);
    assert!(offset_of!(AbiLayoutStateEvent, surface) == 0);
    assert!(offset_of!(AbiLayoutStateEvent, node) == 8);
    assert!(offset_of!(AbiLayoutStateEvent, semantic_state_name) == 16);
    assert!(offset_of!(AbiLayoutStateEvent, semantic_state) == 32);
    assert!(offset_of!(AbiLayoutStateEvent, opaque_runtime_state) == 72);
    assert!(size_of::<AbiModelRangeRequest>() == 48);
    assert!(align_of::<AbiModelRangeRequest>() == 8);
    assert!(offset_of!(AbiModelRangeRequest, request) == 0);
    assert!(offset_of!(AbiModelRangeRequest, model) == 8);
    assert!(offset_of!(AbiModelRangeRequest, start) == 16);
    assert!(offset_of!(AbiModelRangeRequest, count) == 24);
    assert!(offset_of!(AbiModelRangeRequest, sort_filter_token) == 32);
    assert!(size_of::<AbiModelChildrenRequest>() == 56);
    assert!(align_of::<AbiModelChildrenRequest>() == 8);
    assert!(offset_of!(AbiModelChildrenRequest, request) == 0);
    assert!(offset_of!(AbiModelChildrenRequest, model) == 8);
    assert!(offset_of!(AbiModelChildrenRequest, parent_key) == 16);
    assert!(size_of::<AbiRuntimeEventArgs>() == 88);
    assert!(align_of::<AbiRuntimeEventArgs>() == 8);
    assert!(size_of::<AbiRuntimeEvent>() == 96);
    assert!(align_of::<AbiRuntimeEvent>() == 8);
    assert!(offset_of!(AbiRuntimeEvent, kind) == 0);
    assert!(offset_of!(AbiRuntimeEvent, as_) == 8);
    assert!(size_of::<AbiClientApi>() == 72);
    assert!(align_of::<AbiClientApi>() == 8);
    assert!(offset_of!(AbiClientApi, abi_major) == 0);
    assert!(offset_of!(AbiClientApi, abi_minor) == 4);
    assert!(offset_of!(AbiClientApi, context) == 8);
    assert!(offset_of!(AbiClientApi, log) == 16);
    assert!(offset_of!(AbiClientApi, emit_runtime_event) == 24);
    assert!(offset_of!(AbiClientApi, complete_model_request) == 32);
    assert!(offset_of!(AbiClientApi, fail_model_request) == 40);
    assert!(offset_of!(AbiClientApi, read_action_metadata) == 48);
    assert!(offset_of!(AbiClientApi, read_value_debug_json) == 56);
    assert!(offset_of!(AbiClientApi, monotonic_time_ns) == 64);
    assert!(size_of::<AbiRuntimeCreateOptions>() == 88);
    assert!(align_of::<AbiRuntimeCreateOptions>() == 8);
    assert!(offset_of!(AbiRuntimeCreateOptions, client) == 0);
    assert!(offset_of!(AbiRuntimeCreateOptions, locale) == 8);
    assert!(offset_of!(AbiRuntimeCreateOptions, timezone) == 24);
    assert!(offset_of!(AbiRuntimeCreateOptions, theme) == 40);
    assert!(offset_of!(AbiRuntimeCreateOptions, accessibility_preferences_json) == 56);
    assert!(offset_of!(AbiRuntimeCreateOptions, runtime_configuration_json) == 72);
    assert!(size_of::<AbiSurfaceCreateOptions>() == 64);
    assert!(align_of::<AbiSurfaceCreateOptions>() == 8);
    assert!(offset_of!(AbiSurfaceCreateOptions, surface_kind) == 0);
    assert!(offset_of!(AbiSurfaceCreateOptions, title) == 16);
    assert!(offset_of!(AbiSurfaceCreateOptions, state_profile) == 32);
    assert!(offset_of!(AbiSurfaceCreateOptions, opaque_runtime_restore_state) == 48);
    assert!(size_of::<RuntimeApi>() == 120);
    assert!(align_of::<RuntimeApi>() == 8);
    assert!(offset_of!(RuntimeApi, abi_major) == 0);
    assert!(offset_of!(RuntimeApi, abi_minor) == 4);
    assert!(offset_of!(RuntimeApi, describe) == 8);
    assert!(offset_of!(RuntimeApi, create) == 16);
    assert!(offset_of!(RuntimeApi, destroy) == 24);
    assert!(offset_of!(RuntimeApi, start_event_loop) == 32);
    assert!(offset_of!(RuntimeApi, poll_event_loop) == 40);
    assert!(offset_of!(RuntimeApi, request_shutdown) == 48);
    assert!(offset_of!(RuntimeApi, create_surface) == 56);
    assert!(offset_of!(RuntimeApi, destroy_surface) == 64);
    assert!(offset_of!(RuntimeApi, apply_ui_batch) == 72);
    assert!(offset_of!(RuntimeApi, set_surface_visible) == 80);
    assert!(offset_of!(RuntimeApi, capture_semantic_state) == 88);
    assert!(offset_of!(RuntimeApi, capture_opaque_state) == 96);
    assert!(offset_of!(RuntimeApi, apply_model_rows) == 104);
    assert!(offset_of!(RuntimeApi, cancel_request) == 112);
};

// ---------------------------------------------------------------------------
// Owned descriptor metadata and loader
// ---------------------------------------------------------------------------

/// A copied, owned contract offer from a loaded runtime descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeContract {
    pub name: String,
    pub major: u32,
    pub minor: u32,
    pub features: Vec<String>,
}

/// A copied, owned sink offer from a loaded runtime descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSink {
    pub type_name: String,
    pub media_types: Vec<String>,
    pub supports_streaming: bool,
    pub preference_rank: i32,
}

/// A copied, owned runtime descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDescriptor {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub runtime_name: String,
    pub runtime_version: String,
    pub build_id: String,
    pub platform: String,
    pub thread_model: AbiThreadModel,
    pub features: u64,
    pub sinks: Vec<RuntimeSink>,
    pub contracts: Vec<RuntimeContract>,
}

/// Redacted failure from loading or validating a native runtime.
///
/// The loader intentionally does not retain the path or the underlying
/// operating-system/dynamic-loader error.  A caller can log this category
/// without disclosing local filesystem layout or loader internals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeLoadError {
    /// The current target is not the first provider's supported platform.
    UnsupportedPlatform,
    /// The shared library could not be opened.
    LibraryUnavailable,
    /// The required query symbol was not found.
    QuerySymbolUnavailable,
    /// The query symbol itself yielded a null or misaligned table pointer.
    NullApi,
    /// The API table does not match ABI v1.0.
    ApiAbiMismatch,
    /// At least one required API function pointer is null.
    MissingApiFunction,
    /// The API's descriptor function yielded a null or misaligned pointer.
    NullDescriptor,
    /// A descriptor pointer, string, or array was malformed.
    MalformedDescriptor,
    /// The descriptor ABI does not match ABI v1.0.
    DescriptorAbiMismatch,
    /// The descriptor is not the accepted Qt runtime identity.
    DescriptorIdentityMismatch,
    /// The descriptor advertises an unsupported platform.
    DescriptorPlatformMismatch,
    /// The descriptor advertises an unsupported thread model.
    DescriptorThreadModelMismatch,
    /// The descriptor advertises an unsupported feature set.
    DescriptorFeatureMismatch,
    /// The descriptor's sink offers are not exactly the accepted Qt offer.
    DescriptorSinkOffersMismatch,
    /// The descriptor's structural contract offers are not exactly accepted.
    DescriptorContractOffersMismatch,
    /// A descriptor string or array exceeds a client safety bound.
    DescriptorLimitExceeded,
}

impl fmt::Display for RuntimeLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedPlatform => "the Qt runtime is unsupported on this platform",
            Self::LibraryUnavailable => "the Qt runtime library is unavailable",
            Self::QuerySymbolUnavailable => "the Qt runtime query symbol is unavailable",
            Self::NullApi => "the Qt runtime returned no API table",
            Self::ApiAbiMismatch => "the Qt runtime API ABI is incompatible",
            Self::MissingApiFunction => "the Qt runtime API is incomplete",
            Self::NullDescriptor => "the Qt runtime returned no descriptor",
            Self::MalformedDescriptor => "the Qt runtime descriptor is malformed",
            Self::DescriptorAbiMismatch => "the Qt runtime descriptor ABI is incompatible",
            Self::DescriptorIdentityMismatch => "the Qt runtime identity is incompatible",
            Self::DescriptorPlatformMismatch => "the Qt runtime descriptor platform is unsupported",
            Self::DescriptorThreadModelMismatch => {
                "the Qt runtime descriptor thread model is unsupported"
            }
            Self::DescriptorFeatureMismatch => "the Qt runtime descriptor features are unsupported",
            Self::DescriptorSinkOffersMismatch => "the Qt runtime sink offers are incompatible",
            Self::DescriptorContractOffersMismatch => {
                "the Qt runtime contract offers are incompatible"
            }
            Self::DescriptorLimitExceeded => "the Qt runtime descriptor exceeds client limits",
        })
    }
}

impl std::error::Error for RuntimeLoadError {}

/// A validated Qt runtime shared library.
///
/// The copied [`RuntimeApi`] contains typed ABI function pointers rather than
/// `libloading::Symbol` values.  The private [`Library`] field remains part of
/// this owner for the entire lifetime of those pointers; no API table or
/// descriptor is exposed without the owner alive.
pub struct RuntimeLibrary {
    // The copied function pointers are valid only while this owner remains
    // alive.  Keep the Library in the owner even though no raw handle or
    // Symbol is exposed.
    #[allow(dead_code)]
    library: Library,
    api: RuntimeApi,
    descriptor: RuntimeDescriptor,
}

impl RuntimeLibrary {
    /// Loads and validates the installed Qt v1 runtime at `path`.
    ///
    /// This method is deliberately path-only.  It has no database, catalogue,
    /// or runtime-offer selection input, so a database plan cannot cause native
    /// code to be loaded.
    pub fn load_qt(path: impl AsRef<Path>) -> Result<Self, RuntimeLoadError> {
        if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            return Err(RuntimeLoadError::UnsupportedPlatform);
        }
        // SAFETY: loading a native library executes its initializers.  This
        // loader is the explicit trust boundary for an installed,
        // authenticated runtime package; no untrusted database path reaches
        // this method.  See libloading 0.9.0's safety contract:
        // https://docs.rs/libloading/0.9.0/libloading/struct.Library.html
        // The owner is retained below for all copied symbols.
        let library = unsafe { Library::new(path.as_ref()) }
            .map_err(|_| RuntimeLoadError::LibraryUnavailable)?;

        // SAFETY: the symbol name is NUL terminated and the requested type is
        // the exact `extern "C"` signature in the canonical ABI header.
        let query = unsafe { library.get::<QueryFn>(QUERY_SYMBOL) }
            .map_err(|_| RuntimeLoadError::QuerySymbolUnavailable)
            .map(|symbol| *symbol)?;
        if (query as usize) == 0 {
            return Err(RuntimeLoadError::QuerySymbolUnavailable);
        }

        // SAFETY: `query` came from the loaded library with the exact ABI
        // signature above.  Its returned pointer is checked before any table
        // field is read.
        let api_pointer = unsafe { query() };
        if api_pointer.is_null() || !api_pointer.is_aligned() {
            return Err(RuntimeLoadError::NullApi);
        }

        // SAFETY: the pointer is non-null and aligned, and the query contract
        // says it points to an initialized `RuntimeApi` for the library's
        // lifetime.  Copying the table does not dereference any nested pointer.
        let api = unsafe { *api_pointer };
        validate_api(&api)?;

        // SAFETY: `describe` was checked for non-null by `validate_api` and
        // has the exact function-pointer type from the canonical header.
        let descriptor_pointer = unsafe { (api.describe)() };
        let descriptor = copy_descriptor(descriptor_pointer)?;

        Ok(Self {
            library,
            api,
            descriptor,
        })
    }

    /// Returns the copied, typed ABI function table.
    ///
    /// The returned reference is tied to this owner.  Function pointers are
    /// unsafe to call and must not be retained or used after this library is
    /// dropped.
    pub fn api(&self) -> &RuntimeApi {
        &self.api
    }

    /// Returns copied descriptor metadata with no borrowed native pointers.
    pub fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }
}

fn validate_api(api: &RuntimeApi) -> Result<(), RuntimeLoadError> {
    if api.abi_major != ABI_V1_MAJOR || api.abi_minor != ABI_V1_MINOR {
        return Err(RuntimeLoadError::ApiAbiMismatch);
    }

    macro_rules! require_function {
        ($field:ident) => {
            if (api.$field as usize) == 0 {
                return Err(RuntimeLoadError::MissingApiFunction);
            }
        };
    }

    require_function!(describe);
    require_function!(create);
    require_function!(destroy);
    require_function!(start_event_loop);
    require_function!(poll_event_loop);
    require_function!(request_shutdown);
    require_function!(create_surface);
    require_function!(destroy_surface);
    require_function!(apply_ui_batch);
    require_function!(set_surface_visible);
    require_function!(capture_semantic_state);
    require_function!(capture_opaque_state);
    require_function!(apply_model_rows);
    require_function!(cancel_request);

    Ok(())
}

fn copy_descriptor(pointer: *const AbiDescriptor) -> Result<RuntimeDescriptor, RuntimeLoadError> {
    if pointer.is_null() || !pointer.is_aligned() {
        return Err(RuntimeLoadError::NullDescriptor);
    }

    // SAFETY: the caller supplied a non-null, aligned pointer returned by the
    // validated runtime's `describe` function.  Every nested pointer is
    // checked by the bounded readers below before it is dereferenced.
    let descriptor = unsafe { &*pointer };
    if descriptor.abi_major != ABI_V1_MAJOR || descriptor.abi_minor != ABI_V1_MINOR {
        return Err(RuntimeLoadError::DescriptorAbiMismatch);
    }

    let runtime_name = copy_string(descriptor.runtime_name)?;
    let runtime_version = copy_string(descriptor.runtime_version)?;
    let build_id = copy_string(descriptor.build_id)?;
    let platform = copy_string(descriptor.platform)?;

    if runtime_name != QT_RUNTIME_NAME || runtime_version.is_empty() || build_id.is_empty() {
        return Err(RuntimeLoadError::DescriptorIdentityMismatch);
    }
    if platform != QT_PLATFORM {
        return Err(RuntimeLoadError::DescriptorPlatformMismatch);
    }
    if descriptor.thread_model != AbiThreadModel::CALLER_PUMPS {
        return Err(RuntimeLoadError::DescriptorThreadModelMismatch);
    }
    if descriptor.features != RUNTIME_FEATURE_MULTIPLE_WINDOWS {
        return Err(RuntimeLoadError::DescriptorFeatureMismatch);
    }

    let sinks = copy_sinks(descriptor)?;
    let contracts = copy_contracts(descriptor)?;

    Ok(RuntimeDescriptor {
        abi_major: descriptor.abi_major,
        abi_minor: descriptor.abi_minor,
        runtime_name,
        runtime_version,
        build_id,
        platform,
        thread_model: descriptor.thread_model,
        features: descriptor.features,
        sinks,
        contracts,
    })
}

fn copy_sinks(descriptor: &AbiDescriptor) -> Result<Vec<RuntimeSink>, RuntimeLoadError> {
    validate_array_pointer(
        descriptor.sinks,
        descriptor.sink_count,
        CLIENT_MAX_SINK_OFFERS,
    )?;
    if descriptor.sink_count != 1 {
        return Err(RuntimeLoadError::DescriptorSinkOffersMismatch);
    }

    // SAFETY: `validate_array_pointer` checked non-null/alignment and the
    // count is bounded to one before this slice is formed.
    let raw_sinks = unsafe { slice::from_raw_parts(descriptor.sinks, descriptor.sink_count) };
    let raw_sink = raw_sinks[0];
    let type_name = copy_string(raw_sink.type_name)?;
    let media_types = copy_string_array(
        raw_sink.media_types,
        raw_sink.media_type_count,
        CLIENT_MAX_SINK_MEDIA_TYPES,
    )?;
    if type_name != UI_SINK_NAME
        || raw_sink.supports_streaming != 0
        || raw_sink.preference_rank != 0
        || !media_types.is_empty()
    {
        return Err(RuntimeLoadError::DescriptorSinkOffersMismatch);
    }

    Ok(vec![RuntimeSink {
        type_name,
        media_types,
        supports_streaming: raw_sink.supports_streaming != 0,
        preference_rank: raw_sink.preference_rank,
    }])
}

fn copy_contracts(descriptor: &AbiDescriptor) -> Result<Vec<RuntimeContract>, RuntimeLoadError> {
    validate_array_pointer(
        descriptor.contracts,
        descriptor.contract_count,
        CLIENT_MAX_CONTRACT_OFFERS,
    )?;
    if descriptor.contract_count != CLIENT_MAX_CONTRACT_OFFERS {
        return Err(RuntimeLoadError::DescriptorContractOffersMismatch);
    }

    // SAFETY: `validate_array_pointer` checked non-null/alignment and bounded
    // the count before this slice is formed.
    let raw_contracts =
        unsafe { slice::from_raw_parts(descriptor.contracts, descriptor.contract_count) };
    let mut seen = HashSet::with_capacity(REQUIRED_CONTRACTS.len());
    let mut contracts = Vec::with_capacity(raw_contracts.len());
    for raw_contract in raw_contracts {
        let name = copy_string(raw_contract.name)?;
        let features = copy_string_array(
            raw_contract.features,
            raw_contract.feature_count,
            CLIENT_MAX_CONTRACT_FEATURES,
        )?;
        let expected = REQUIRED_CONTRACTS
            .iter()
            .find(|(candidate, _, _)| *candidate == name);
        let Some((_, expected_major, expected_minor)) = expected else {
            return Err(RuntimeLoadError::DescriptorContractOffersMismatch);
        };
        if raw_contract.major != *expected_major
            || raw_contract.minor != *expected_minor
            || !features.is_empty()
            || !seen.insert(name.clone())
        {
            return Err(RuntimeLoadError::DescriptorContractOffersMismatch);
        }
        contracts.push(RuntimeContract {
            name,
            major: raw_contract.major,
            minor: raw_contract.minor,
            features,
        });
    }
    if seen.len() != REQUIRED_CONTRACTS.len() {
        return Err(RuntimeLoadError::DescriptorContractOffersMismatch);
    }
    Ok(contracts)
}

fn copy_string_array(
    pointer: *const AbiStringView,
    count: usize,
    maximum: usize,
) -> Result<Vec<String>, RuntimeLoadError> {
    validate_array_pointer(pointer, count, maximum)?;
    if count == 0 {
        return Ok(Vec::new());
    }

    // SAFETY: `validate_array_pointer` checked non-null/alignment and bounded
    // the count before this slice is formed.
    let raw_values = unsafe { slice::from_raw_parts(pointer, count) };
    raw_values.iter().copied().map(copy_string).collect()
}

fn copy_string(view: AbiStringView) -> Result<String, RuntimeLoadError> {
    if view.len > CLIENT_MAX_DESCRIPTOR_STRING_BYTES {
        return Err(RuntimeLoadError::DescriptorLimitExceeded);
    }
    if view.len == 0 {
        return Ok(String::new());
    }
    if view.data.is_null() || !view.data.is_aligned() {
        return Err(RuntimeLoadError::MalformedDescriptor);
    }
    (view.data as usize)
        .checked_add(view.len)
        .ok_or(RuntimeLoadError::MalformedDescriptor)?;

    // SAFETY: the pointer is non-null/aligned and the length was bounded above
    // before constructing this byte slice.  UTF-8 validation happens before
    // allocating an owned String.
    let bytes = unsafe { slice::from_raw_parts(view.data.cast::<u8>(), view.len) };
    let value = str::from_utf8(bytes).map_err(|_| RuntimeLoadError::MalformedDescriptor)?;
    if value.contains('\0') {
        return Err(RuntimeLoadError::MalformedDescriptor);
    }
    Ok(value.to_owned())
}

fn validate_array_pointer<T>(
    pointer: *const T,
    count: usize,
    maximum: usize,
) -> Result<(), RuntimeLoadError> {
    if count > maximum {
        return Err(RuntimeLoadError::DescriptorLimitExceeded);
    }
    if count == 0 {
        return Ok(());
    }
    if pointer.is_null() || !pointer.is_aligned() {
        return Err(RuntimeLoadError::MalformedDescriptor);
    }
    let byte_length = count
        .checked_mul(std::mem::size_of::<T>())
        .filter(|length| *length <= isize::MAX as usize)
        .ok_or(RuntimeLoadError::DescriptorLimitExceeded)?;
    (pointer as usize)
        .checked_add(byte_length)
        .ok_or(RuntimeLoadError::MalformedDescriptor)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    fn view(value: &'static str) -> AbiStringView {
        AbiStringView {
            data: value.as_ptr().cast::<c_char>(),
            len: value.len(),
        }
    }

    struct DescriptorFixture {
        descriptor: AbiDescriptor,
        #[allow(dead_code)]
        sinks: Vec<AbiSinkOffer>,
        contracts: Vec<AbiContractVersion>,
    }

    fn valid_descriptor() -> DescriptorFixture {
        let sinks = vec![AbiSinkOffer {
            type_name: view(UI_SINK_NAME),
            media_types: ptr::null(),
            media_type_count: 0,
            supports_streaming: 0,
            preference_rank: 0,
        }];
        let contracts = vec![
            AbiContractVersion {
                name: view("std.ui.window"),
                major: 1,
                minor: 0,
                features: ptr::null(),
                feature_count: 0,
            },
            AbiContractVersion {
                name: view("std.ui.text"),
                major: 1,
                minor: 0,
                features: ptr::null(),
                feature_count: 0,
            },
            AbiContractVersion {
                name: view("std.ui.button"),
                major: 1,
                minor: 0,
                features: ptr::null(),
                feature_count: 0,
            },
            AbiContractVersion {
                name: view("std.ui.panel"),
                major: 1,
                minor: 0,
                features: ptr::null(),
                feature_count: 0,
            },
            AbiContractVersion {
                name: view("std.ui.row"),
                major: 1,
                minor: 0,
                features: ptr::null(),
                feature_count: 0,
            },
            AbiContractVersion {
                name: view("std.ui.column"),
                major: 1,
                minor: 0,
                features: ptr::null(),
                feature_count: 0,
            },
            AbiContractVersion {
                name: view("std.ui.text_input"),
                major: 1,
                minor: 0,
                features: ptr::null(),
                feature_count: 0,
            },
            AbiContractVersion {
                name: view("std.ui.tabs"),
                major: 1,
                minor: 0,
                features: ptr::null(),
                feature_count: 0,
            },
        ];
        let descriptor = AbiDescriptor {
            abi_major: ABI_V1_MAJOR,
            abi_minor: ABI_V1_MINOR,
            runtime_name: view(QT_RUNTIME_NAME),
            runtime_version: view("1.0.0"),
            build_id: view("test-build"),
            platform: view(QT_PLATFORM),
            thread_model: AbiThreadModel::CALLER_PUMPS,
            features: RUNTIME_FEATURE_MULTIPLE_WINDOWS,
            sinks: sinks.as_ptr(),
            sink_count: sinks.len(),
            contracts: contracts.as_ptr(),
            contract_count: contracts.len(),
        };
        DescriptorFixture {
            descriptor,
            sinks,
            contracts,
        }
    }

    #[test]
    fn copies_the_accepted_qt_descriptor_without_borrowed_metadata() {
        let fixture = valid_descriptor();
        let descriptor = copy_descriptor(&fixture.descriptor).expect("valid descriptor");

        assert_eq!(descriptor.runtime_name, QT_RUNTIME_NAME);
        assert_eq!(descriptor.runtime_version, "1.0.0");
        assert_eq!(descriptor.platform, QT_PLATFORM);
        assert_eq!(descriptor.sinks.len(), 1);
        assert_eq!(descriptor.contracts.len(), REQUIRED_CONTRACTS.len());
        assert!(
            descriptor
                .contracts
                .iter()
                .all(|contract| contract.features.is_empty())
        );
    }

    #[test]
    fn rejects_null_string_data_before_reading() {
        let mut fixture = valid_descriptor();
        fixture.descriptor.build_id = AbiStringView {
            data: ptr::null(),
            len: 1,
        };

        assert_eq!(
            copy_descriptor(&fixture.descriptor),
            Err(RuntimeLoadError::MalformedDescriptor)
        );
    }

    #[test]
    fn rejects_over_capacity_array_before_reading() {
        let mut fixture = valid_descriptor();
        fixture.descriptor.contract_count = usize::MAX;
        fixture.descriptor.contracts = ptr::null();

        assert_eq!(
            copy_descriptor(&fixture.descriptor),
            Err(RuntimeLoadError::DescriptorLimitExceeded)
        );
    }

    #[test]
    fn rejects_unknown_or_missing_structural_contracts() {
        let mut fixture = valid_descriptor();
        fixture.contracts[0].name = view("std.ui.unknown");

        assert_eq!(
            copy_descriptor(&fixture.descriptor),
            Err(RuntimeLoadError::DescriptorContractOffersMismatch)
        );
    }
}
