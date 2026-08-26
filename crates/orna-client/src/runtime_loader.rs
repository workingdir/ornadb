//! Trusted loading and descriptor validation for the production Qt runtime.
//!
//! The ABI declarations in this module intentionally mirror
//! `spec/spec/orna_runtime_abi_v1.h`.  The raw declarations are only used at
//! the dynamic-library boundary.  [`RuntimeLibrary`] copies the API table and
//! descriptor into Rust-owned values while retaining the [`libloading::Library`]
//! that keeps every copied function pointer valid.
//!
//! [`RuntimeSession`] implements the caller-pumps side of this boundary:
//! creation and all surface operations, polling, callbacks, shutdown, and
//! destruction must remain on the client-owned runtime thread.  Callback
//! snapshots are copied before the native callback returns and contain no
//! borrowed native pointers.

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
        validate_native_range(api_pointer, std::mem::size_of::<RuntimeApi>())
            .map_err(|_| RuntimeLoadError::NullApi)?;

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
    validate_native_range(pointer, std::mem::size_of::<AbiDescriptor>())
        .map_err(|_| RuntimeLoadError::NullDescriptor)?;

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

fn validate_native_range<T>(pointer: *const T, byte_length: usize) -> Result<(), RuntimeLoadError> {
    if byte_length > isize::MAX as usize {
        return Err(RuntimeLoadError::DescriptorLimitExceeded);
    }
    (pointer as usize)
        .checked_add(byte_length)
        .ok_or(RuntimeLoadError::MalformedDescriptor)?;
    Ok(())
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
    validate_native_range(view.data, view.len)?;

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
    validate_native_range(pointer, byte_length)?;
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
// ---------------------------------------------------------------------------
// Caller-pumps session boundary
// ---------------------------------------------------------------------------

/// Maximum number of bytes accepted for one client-supplied runtime string.
///
/// The Qt provider uses the same bound for locale, timezone, theme, and the
/// surface text fields.  Configuration JSON has a separate payload-sized
/// bound below.
pub const CLIENT_MAX_RUNTIME_TEXT_BYTES: usize = CLIENT_MAX_DESCRIPTOR_STRING_BYTES;
/// Maximum bytes accepted for runtime configuration JSON.
pub const CLIENT_MAX_RUNTIME_CONFIGURATION_BYTES: usize = 16 * 1024 * 1024;
/// Maximum bytes copied from one runtime value or semantic capture.
pub const CLIENT_MAX_RUNTIME_VALUE_BYTES: usize = CLIENT_MAX_RUNTIME_CONFIGURATION_BYTES;
/// Maximum operations submitted in one UI batch.
pub const CLIENT_MAX_RUNTIME_BATCH_OPERATIONS: usize = 1024;
/// Maximum queued callback events retained by one session.
pub const CLIENT_MAX_QUEUED_RUNTIME_EVENTS: usize = 1024;
/// Maximum bytes retained by one session's callback event queue.
pub const CLIENT_MAX_QUEUED_RUNTIME_EVENT_BYTES: usize = CLIENT_MAX_RUNTIME_VALUE_BYTES;

/// A value copied from a runtime callback without retaining native pointers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeValueSnapshot {
    /// The runtime's value handle, when the event supplied one.
    pub handle: AbiHandle,
    /// The bounded UTF-8 type name supplied by the runtime.
    pub type_name: String,
    /// The canonical value bytes copied during the callback.
    pub canonical_encoding: Vec<u8>,
}

/// An action callback event copied into owned Rust data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeActionEventSnapshot {
    pub surface: AbiSurfaceHandle,
    pub node: AbiNodeHandle,
    pub action: AbiActionHandle,
    pub payload: RuntimeValueSnapshot,
}

/// A surface-closed callback event copied into owned Rust data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeSurfaceClosedEventSnapshot {
    pub surface: AbiSurfaceHandle,
}

/// A diagnostic callback event copied into owned Rust data.
///
/// The status code is the stable machine-readable part of the diagnostic.
/// The message is copied and bounded so no native string view escapes the
/// callback.  [`RuntimeSessionError`] deliberately does not retain this
/// message when mapping a failed ABI call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDiagnosticEventSnapshot {
    pub code: AbiStatusCode,
    pub message: String,
}

/// Owned event snapshots exposed by [`RuntimeSession`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEventSnapshot {
    Action(RuntimeActionEventSnapshot),
    SurfaceClosed(RuntimeSurfaceClosedEventSnapshot),
    Diagnostic(RuntimeDiagnosticEventSnapshot),
}

/// Short aliases for callers that prefer the event names without the
/// `Snapshot` suffix.
pub type RuntimeEvent = RuntimeEventSnapshot;
pub type RuntimeActionEvent = RuntimeActionEventSnapshot;
pub type RuntimeSurfaceClosedEvent = RuntimeSurfaceClosedEventSnapshot;
pub type RuntimeDiagnosticEvent = RuntimeDiagnosticEventSnapshot;

/// Surface creation inputs accepted by the safe session wrapper.
///
/// The borrowed fields are converted to ABI views only for the duration of
/// [`RuntimeSession::create_surface_with_options`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeSurfaceOptions<'a> {
    pub surface_kind: &'a str,
    pub title: &'a str,
    pub state_profile: &'a str,
    pub opaque_runtime_restore_state: &'a [u8],
}

impl<'a> RuntimeSurfaceOptions<'a> {
    /// Constructs surface options from the four ABI fields.
    pub const fn new(
        surface_kind: &'a str,
        title: &'a str,
        state_profile: &'a str,
        opaque_runtime_restore_state: &'a [u8],
    ) -> Self {
        Self {
            surface_kind,
            title,
            state_profile,
            opaque_runtime_restore_state,
        }
    }
}

impl<'a> Default for RuntimeSurfaceOptions<'a> {
    fn default() -> Self {
        Self {
            surface_kind: "window",
            title: "",
            state_profile: "",
            opaque_runtime_restore_state: &[],
        }
    }
}

/// A client-owned value used as an input to an owned UI batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeValueInput {
    pub handle: AbiHandle,
    pub type_name: String,
    pub canonical_encoding: Vec<u8>,
}

impl RuntimeValueInput {
    /// Creates a value input. Bounds are enforced when the batch is applied.
    pub fn new(
        handle: AbiHandle,
        type_name: impl Into<String>,
        canonical_encoding: Vec<u8>,
    ) -> Self {
        Self {
            handle,
            type_name: type_name.into(),
            canonical_encoding,
        }
    }

    /// Creates the null/empty value reference used by unkeyed mounts and
    /// property clears.
    pub fn empty() -> Self {
        Self {
            handle: 0,
            type_name: String::new(),
            canonical_encoding: Vec::new(),
        }
    }
}

impl Default for RuntimeValueInput {
    fn default() -> Self {
        Self::empty()
    }
}

/// One client-owned UI operation lowered to the v1 ABI at call time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeUiOperation {
    MountNode {
        node: AbiNodeHandle,
        parent: AbiNodeHandle,
        slot: String,
        ordinal: usize,
        contract_name: String,
        contract_major: u32,
        contract_minor: u32,
        explicit_key: RuntimeValueInput,
    },
    UnmountNode {
        node: AbiNodeHandle,
    },
    SetProperty {
        node: AbiNodeHandle,
        property: String,
        value: RuntimeValueInput,
    },
    ClearProperty {
        node: AbiNodeHandle,
        property: String,
    },
    InsertChild {
        parent: AbiNodeHandle,
        slot: String,
        child: AbiNodeHandle,
        ordinal: usize,
    },
    RemoveChild {
        parent: AbiNodeHandle,
        slot: String,
        child: AbiNodeHandle,
        ordinal: usize,
    },
    MoveChild {
        parent: AbiNodeHandle,
        slot: String,
        child: AbiNodeHandle,
        ordinal: usize,
    },
    BindAction {
        node: AbiNodeHandle,
        event_name: String,
        action: AbiActionHandle,
        input_type: String,
    },
    UnbindAction {
        node: AbiNodeHandle,
        event_name: String,
        action: AbiActionHandle,
    },
    /// Retained to make unsupported ABI operations explicit to callers.
    SetFocus,
    /// Retained to make unsupported ABI operations explicit to callers.
    SetAccessibility,
}

impl RuntimeUiOperation {
    pub fn mount_node(
        node: AbiNodeHandle,
        parent: AbiNodeHandle,
        slot: impl Into<String>,
        ordinal: usize,
        contract_name: impl Into<String>,
        contract_major: u32,
        contract_minor: u32,
        explicit_key: RuntimeValueInput,
    ) -> Self {
        Self::MountNode {
            node,
            parent,
            slot: slot.into(),
            ordinal,
            contract_name: contract_name.into(),
            contract_major,
            contract_minor,
            explicit_key,
        }
    }

    pub fn unmount_node(node: AbiNodeHandle) -> Self {
        Self::UnmountNode { node }
    }

    pub fn set_property(
        node: AbiNodeHandle,
        property: impl Into<String>,
        value: RuntimeValueInput,
    ) -> Self {
        Self::SetProperty {
            node,
            property: property.into(),
            value,
        }
    }

    pub fn clear_property(node: AbiNodeHandle, property: impl Into<String>) -> Self {
        Self::ClearProperty {
            node,
            property: property.into(),
        }
    }

    pub fn insert_child(
        parent: AbiNodeHandle,
        slot: impl Into<String>,
        child: AbiNodeHandle,
        ordinal: usize,
    ) -> Self {
        Self::InsertChild {
            parent,
            slot: slot.into(),
            child,
            ordinal,
        }
    }

    pub fn remove_child(
        parent: AbiNodeHandle,
        slot: impl Into<String>,
        child: AbiNodeHandle,
        ordinal: usize,
    ) -> Self {
        Self::RemoveChild {
            parent,
            slot: slot.into(),
            child,
            ordinal,
        }
    }

    pub fn move_child(
        parent: AbiNodeHandle,
        slot: impl Into<String>,
        child: AbiNodeHandle,
        ordinal: usize,
    ) -> Self {
        Self::MoveChild {
            parent,
            slot: slot.into(),
            child,
            ordinal,
        }
    }

    pub fn bind_action(
        node: AbiNodeHandle,
        event_name: impl Into<String>,
        action: AbiActionHandle,
        input_type: impl Into<String>,
    ) -> Self {
        Self::BindAction {
            node,
            event_name: event_name.into(),
            action,
            input_type: input_type.into(),
        }
    }

    pub fn unbind_action(
        node: AbiNodeHandle,
        event_name: impl Into<String>,
        action: AbiActionHandle,
    ) -> Self {
        Self::UnbindAction {
            node,
            event_name: event_name.into(),
            action,
        }
    }
}

/// A bounded, client-owned UI batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeUiBatch {
    pub semantic_revision: u64,
    pub operations: Vec<RuntimeUiOperation>,
}

impl RuntimeUiBatch {
    pub fn new(semantic_revision: u64) -> Self {
        Self {
            semantic_revision,
            operations: Vec::new(),
        }
    }

    /// Appends an operation while enforcing the provider's batch bound.
    pub fn push(&mut self, operation: RuntimeUiOperation) -> Result<(), RuntimeSessionError> {
        if self.operations.len() >= CLIENT_MAX_RUNTIME_BATCH_OPERATIONS {
            return Err(RuntimeSessionError::InvalidArgument);
        }
        self.operations.push(operation);
        Ok(())
    }

    pub fn with_operations(
        semantic_revision: u64,
        operations: Vec<RuntimeUiOperation>,
    ) -> Result<Self, RuntimeSessionError> {
        if operations.len() > CLIENT_MAX_RUNTIME_BATCH_OPERATIONS {
            return Err(RuntimeSessionError::InvalidArgument);
        }
        Ok(Self {
            semantic_revision,
            operations,
        })
    }
}

/// Redacted error returned by the safe runtime session boundary.
///
/// Native status messages are intentionally ignored.  They are runtime-owned
/// pointers and can contain implementation details; the ABI status code is
/// the only native failure detail retained here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeSessionError {
    /// The runtime library failed the loader's trust and ABI checks.
    Load(RuntimeLoadError),
    /// A caller-supplied locale, timezone, theme, or configuration string
    /// exceeded the corresponding bound.
    InvalidConfiguration,
    /// The ABI rejected an argument or an output was malformed.
    InvalidArgument,
    /// The runtime does not implement the requested operation.
    Unsupported,
    /// A surface or other runtime object was not found.
    NotFound,
    /// The caller-pumps runtime rejected an operation during a callback.
    Busy,
    /// A request was cancelled by the runtime.
    Cancelled,
    /// The runtime reported an operation or toolkit failure.
    Failed,
    /// The runtime reported an invariant or allocation failure.
    Internal,
    /// A batch revision was not strictly newer than the committed revision.
    StaleRevision,
    /// The runtime returned a status code not defined by ABI v1.
    UnknownStatus(i32),
    /// The session has already destroyed its runtime.
    Destroyed,
    /// The runtime returned an owned-byte descriptor that cannot be copied and
    /// released safely.
    MalformedOwnedBytes,
    /// A best-effort shutdown did not reach terminal state.
    ShutdownIncomplete,
}

impl RuntimeSessionError {
    fn from_status(status: AbiStatus) -> Result<(), Self> {
        match status.code.0 {
            0 => Ok(()),
            1 => Err(Self::InvalidArgument),
            2 => Err(Self::Unsupported),
            3 => Err(Self::NotFound),
            4 => Err(Self::Busy),
            5 => Err(Self::Cancelled),
            6 => Err(Self::Failed),
            7 => Err(Self::Internal),
            8 => Err(Self::StaleRevision),
            code => Err(Self::UnknownStatus(code)),
        }
    }

    /// Returns the ABI status code represented by this error, when one
    /// exists.  Runtime loader and local lifecycle errors have no ABI code.
    pub const fn status_code(self) -> Option<AbiStatusCode> {
        match self {
            Self::InvalidArgument => Some(AbiStatusCode::INVALID_ARGUMENT),
            Self::Unsupported => Some(AbiStatusCode::UNSUPPORTED),
            Self::NotFound => Some(AbiStatusCode::NOT_FOUND),
            Self::Busy => Some(AbiStatusCode::BUSY),
            Self::Cancelled => Some(AbiStatusCode::CANCELLED),
            Self::Failed => Some(AbiStatusCode::FAILED),
            Self::Internal => Some(AbiStatusCode::INTERNAL),
            Self::StaleRevision => Some(AbiStatusCode::STALE_REVISION),
            Self::UnknownStatus(code) => Some(AbiStatusCode(code)),
            Self::Load(_)
            | Self::InvalidConfiguration
            | Self::Destroyed
            | Self::MalformedOwnedBytes
            | Self::ShutdownIncomplete => None,
        }
    }
}

impl From<RuntimeLoadError> for RuntimeSessionError {
    fn from(error: RuntimeLoadError) -> Self {
        Self::Load(error)
    }
}

impl fmt::Display for RuntimeSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Load(error) => return error.fmt(formatter),
            Self::InvalidConfiguration => "the runtime session configuration is invalid",
            Self::InvalidArgument => "the runtime rejected a session argument",
            Self::Unsupported => "the runtime does not support the requested operation",
            Self::NotFound => "the runtime object was not found",
            Self::Busy => "the runtime is busy",
            Self::Cancelled => "the runtime operation was cancelled",
            Self::Failed => "the runtime operation failed",
            Self::Internal => "the runtime reported an internal failure",
            Self::StaleRevision => "the runtime UI revision is stale",
            Self::UnknownStatus(_) => "the runtime returned an unknown status",
            Self::Destroyed => "the runtime session has been destroyed",
            Self::MalformedOwnedBytes => "the runtime returned malformed owned bytes",
            Self::ShutdownIncomplete => "the runtime session did not reach terminal shutdown",
        })
    }
}

impl std::error::Error for RuntimeSessionError {}

/// State retained behind the `OrnaClientApiV1.context` pointer.
///
/// The state is boxed before `create` and remains boxed until after a
/// successful terminal shutdown and runtime destruction.  The caller-pumps
/// contract means callbacks and session operations are serialized by the
/// client-owned runtime thread.
struct CallbackState {
    runtime: AbiRuntimeHandle,
    events: std::collections::VecDeque<RuntimeEventSnapshot>,
    queued_bytes: usize,
}

impl CallbackState {
    fn new() -> Self {
        Self {
            runtime: 0,
            events: std::collections::VecDeque::new(),
            queued_bytes: 0,
        }
    }

    fn push_event(&mut self, event: RuntimeEventSnapshot) -> Result<(), AbiStatusCode> {
        if self.events.len() >= CLIENT_MAX_QUEUED_RUNTIME_EVENTS {
            return Err(AbiStatusCode::INTERNAL);
        }
        let event_bytes = runtime_event_size(&event);
        let queued_bytes = self
            .queued_bytes
            .checked_add(event_bytes)
            .ok_or(AbiStatusCode::INTERNAL)?;
        if queued_bytes > CLIENT_MAX_QUEUED_RUNTIME_EVENT_BYTES {
            return Err(AbiStatusCode::INTERNAL);
        }
        self.events
            .try_reserve(1)
            .map_err(|_| AbiStatusCode::INTERNAL)?;
        self.events.push_back(event);
        self.queued_bytes = queued_bytes;
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<RuntimeEventSnapshot> {
        self.queued_bytes = 0;
        self.events.drain(..).collect()
    }
}

fn runtime_event_size(event: &RuntimeEventSnapshot) -> usize {
    match event {
        RuntimeEventSnapshot::Action(action) => action
            .payload
            .canonical_encoding
            .len()
            .saturating_add(action.payload.type_name.len()),
        RuntimeEventSnapshot::SurfaceClosed(_) => 0,
        RuntimeEventSnapshot::Diagnostic(diagnostic) => diagnostic.message.len(),
    }
}

/// A safe owner for one caller-pumps runtime instance.
///
/// The runtime library, copied API table, callback state, and runtime handle
/// are one lifetime domain.  `RuntimeLibrary` is retained until the native
/// runtime has been destroyed, so no copied function pointer outlives its
/// shared library.
pub struct RuntimeSession {
    library: Option<RuntimeLibrary>,
    runtime: AbiRuntimeHandle,
    callback_state: Option<Box<CallbackState>>,
    #[allow(dead_code)]
    client_api: Option<Box<AbiClientApi>>,
    terminal: bool,
    destroyed: bool,
    // The Qt provider binds a runtime to the creating thread.  Keep this
    // owner deliberately !Send and !Sync so safe methods cannot move it
    // across the caller-pumps boundary.
    _owner_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl RuntimeSession {
    /// Creates a Qt session with empty accessibility and runtime
    /// configuration JSON (`{}`).
    pub fn new_qt(
        library: RuntimeLibrary,
        locale: &str,
        timezone: &str,
        theme: &str,
    ) -> Result<Self, RuntimeSessionError> {
        Self::new_qt_with_configs(library, locale, timezone, theme, "{}", "{}")
    }

    /// Creates a Qt session with custom runtime configuration JSON and empty
    /// accessibility preferences (`{}`).
    pub fn new_qt_with_configuration(
        library: RuntimeLibrary,
        locale: &str,
        timezone: &str,
        theme: &str,
        runtime_configuration_json: &str,
    ) -> Result<Self, RuntimeSessionError> {
        Self::new_qt_with_configs(
            library,
            locale,
            timezone,
            theme,
            "{}",
            runtime_configuration_json,
        )
    }

    /// Creates a Qt session with both bounded JSON configuration strings.
    pub fn new_qt_with_configs(
        library: RuntimeLibrary,
        locale: &str,
        timezone: &str,
        theme: &str,
        accessibility_preferences_json: &str,
        runtime_configuration_json: &str,
    ) -> Result<Self, RuntimeSessionError> {
        let locale = runtime_string_view(locale, CLIENT_MAX_RUNTIME_TEXT_BYTES)?;
        let timezone = runtime_string_view(timezone, CLIENT_MAX_RUNTIME_TEXT_BYTES)?;
        let theme = runtime_string_view(theme, CLIENT_MAX_RUNTIME_TEXT_BYTES)?;
        let accessibility_preferences_json = runtime_string_view(
            accessibility_preferences_json,
            CLIENT_MAX_RUNTIME_TEXT_BYTES,
        )?;
        let runtime_configuration_json = runtime_string_view(
            runtime_configuration_json,
            CLIENT_MAX_RUNTIME_CONFIGURATION_BYTES,
        )?;

        let mut callback_state = Box::new(CallbackState::new());
        let context = (callback_state.as_mut() as *mut CallbackState).cast::<c_void>();
        let client_api = Box::new(AbiClientApi {
            abi_major: ABI_V1_MAJOR,
            abi_minor: ABI_V1_MINOR,
            context,
            log: callback_log,
            emit_runtime_event: callback_emit_runtime_event,
            complete_model_request: callback_complete_model_request,
            fail_model_request: callback_fail_model_request,
            read_action_metadata: callback_read_action_metadata,
            read_value_debug_json: callback_read_value_debug_json,
            monotonic_time_ns: callback_monotonic_time_ns,
        });
        let options = AbiRuntimeCreateOptions {
            client: client_api.as_ref() as *const AbiClientApi,
            locale,
            timezone,
            theme,
            accessibility_preferences_json,
            runtime_configuration_json,
        };
        let mut runtime = 0;
        let create = library.api().create;
        // SAFETY: every view in `options` points into a caller-owned `str`
        // that remains alive for this call.  `client_api` points to the boxed
        // callback state, which remains alive for the whole session.
        let status = unsafe { create(&options, &mut runtime) };
        if let Err(error) = RuntimeSessionError::from_status(status) {
            // A conforming provider returns a zero handle on failure.  If a
            // malformed provider violates that rule, do not unload its
            // library or free its callback context/API table while the handle
            // may still be live.
            if runtime != 0 {
                std::mem::forget(library);
                std::mem::forget(callback_state);
                std::mem::forget(client_api);
            }
            return Err(error);
        }
        if runtime == 0 {
            return Err(RuntimeSessionError::Internal);
        }
        callback_state.runtime = runtime;

        Ok(Self {
            library: Some(library),
            runtime,
            callback_state: Some(callback_state),
            client_api: Some(client_api),
            terminal: false,
            destroyed: false,
            _owner_thread: std::marker::PhantomData,
        })
    }

    /// Returns the runtime handle for diagnostics and integration code.
    pub const fn runtime_handle(&self) -> AbiRuntimeHandle {
        self.runtime
    }

    /// Returns whether a successful shutdown has reached terminal state.
    pub const fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Returns whether the native runtime has already been destroyed.
    pub const fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// Returns copied descriptor metadata while the session owns its library.
    pub fn descriptor(&self) -> &RuntimeDescriptor {
        self.library
            .as_ref()
            .expect("a live session retains its runtime library")
            .descriptor()
    }

    /// Processes Qt events on the client-owned runtime thread.
    pub fn poll_event_loop(&mut self, timeout_ms: u32) -> Result<(), RuntimeSessionError> {
        let runtime = self.live_runtime()?;
        let poll_event_loop = self.api().poll_event_loop;
        // SAFETY: `runtime` belongs to this live library owner and the scalar
        // timeout has no borrowed data.
        let status = unsafe { poll_event_loop(runtime, timeout_ms) };
        RuntimeSessionError::from_status(status)
    }

    /// Short alias for [`RuntimeSession::poll_event_loop`].
    pub fn poll(&mut self, timeout_ms: u32) -> Result<(), RuntimeSessionError> {
        self.poll_event_loop(timeout_ms)
    }

    /// Creates the first provider's ordinary window surface.
    pub fn create_surface(&mut self, title: &str) -> Result<AbiSurfaceHandle, RuntimeSessionError> {
        self.create_surface_with_options(&RuntimeSurfaceOptions {
            surface_kind: "window",
            title,
            state_profile: "",
            opaque_runtime_restore_state: &[],
        })
    }

    /// Creates a surface from bounded Rust strings and bytes.
    pub fn create_surface_with_options(
        &mut self,
        options: &RuntimeSurfaceOptions<'_>,
    ) -> Result<AbiSurfaceHandle, RuntimeSessionError> {
        let surface_kind = runtime_string_view(options.surface_kind, CLIENT_MAX_RUNTIME_TEXT_BYTES)
            .map_err(|_| RuntimeSessionError::InvalidArgument)?;
        let title = runtime_string_view(options.title, CLIENT_MAX_RUNTIME_TEXT_BYTES)
            .map_err(|_| RuntimeSessionError::InvalidArgument)?;
        let state_profile =
            runtime_string_view(options.state_profile, CLIENT_MAX_RUNTIME_TEXT_BYTES)
                .map_err(|_| RuntimeSessionError::InvalidArgument)?;
        let opaque_runtime_restore_state = runtime_bytes_view(
            options.opaque_runtime_restore_state,
            CLIENT_MAX_RUNTIME_VALUE_BYTES,
        )
        .map_err(|_| RuntimeSessionError::InvalidArgument)?;
        let abi_options = AbiSurfaceCreateOptions {
            surface_kind,
            title,
            state_profile,
            opaque_runtime_restore_state,
        };
        let runtime = self.live_runtime()?;
        let create_surface = self.api().create_surface;
        let mut surface = 0;
        // SAFETY: all string and byte views point into `options` borrows that
        // remain valid for the duration of this ABI call.
        let status = unsafe { create_surface(runtime, &abi_options, &mut surface) };
        RuntimeSessionError::from_status(status)?;
        if surface == 0 {
            return Err(RuntimeSessionError::Internal);
        }
        Ok(surface)
    }

    /// Destroys a surface and receives its terminal close event, when
    /// supported by the runtime.
    pub fn destroy_surface(
        &mut self,
        surface: AbiSurfaceHandle,
    ) -> Result<(), RuntimeSessionError> {
        let runtime = self.live_runtime()?;
        let destroy_surface = self.api().destroy_surface;
        // SAFETY: both handles are scalar values obtained from this runtime.
        let status = unsafe { destroy_surface(runtime, surface) };
        RuntimeSessionError::from_status(status)
    }

    /// Sets a surface's visibility using the ABI's canonical boolean values.
    pub fn set_surface_visible(
        &mut self,
        surface: AbiSurfaceHandle,
        visible: bool,
    ) -> Result<(), RuntimeSessionError> {
        let runtime = self.live_runtime()?;
        let set_surface_visible = self.api().set_surface_visible;
        // SAFETY: both handles are scalar values obtained from this runtime.
        let status = unsafe { set_surface_visible(runtime, surface, u8::from(visible)) };
        RuntimeSessionError::from_status(status)
    }

    /// Applies a client-owned UI batch.
    ///
    /// Strings and value bytes are checked against the provider bounds and
    /// lowered to temporary ABI views.  The temporary operation array and all
    /// nested views stay alive until the provider returns.
    pub fn apply_batch(
        &mut self,
        surface: AbiSurfaceHandle,
        batch: &RuntimeUiBatch,
    ) -> Result<(), RuntimeSessionError> {
        let prepared = prepare_ui_batch(batch)?;
        let runtime = self.live_runtime()?;
        let apply_ui_batch = self.api().apply_ui_batch;
        // SAFETY: `prepared` owns the operation array and keeps the source
        // batch borrowed while all nested views are passed to the provider.
        let status = unsafe { apply_ui_batch(runtime, surface, &prepared.batch) };
        RuntimeSessionError::from_status(status)
    }

    /// Applies a raw ABI batch.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `batch.operations` is a valid array of
    /// `AbiUiOperation` values and that every string/byte pointer reachable
    /// through those operations is valid for its declared length and remains
    /// alive until this method returns.  The native provider borrows those
    /// views only during the ABI call and must not retain them.
    pub unsafe fn apply_batch_abi(
        &mut self,
        surface: AbiSurfaceHandle,
        batch: &AbiUiBatch,
    ) -> Result<(), RuntimeSessionError> {
        validate_batch_view(batch)?;
        let runtime = self.live_runtime()?;
        let apply_ui_batch = self.api().apply_ui_batch;
        // SAFETY: the function's caller contract establishes that `batch` and
        // all nested views are valid for this call.
        let status = unsafe { apply_ui_batch(runtime, surface, batch) };
        RuntimeSessionError::from_status(status)
    }

    /// Builds a raw ABI batch from a Rust operation slice and applies it.
    ///
    /// # Safety
    ///
    /// Every nested view inside each operation must satisfy the same
    /// lifetime/validity contract as [`RuntimeSession::apply_batch_abi`].
    pub unsafe fn apply_batch_parts(
        &mut self,
        surface: AbiSurfaceHandle,
        semantic_revision: u64,
        operations: &[AbiUiOperation],
    ) -> Result<(), RuntimeSessionError> {
        let batch = AbiUiBatch {
            semantic_revision,
            operations: if operations.is_empty() {
                std::ptr::null()
            } else {
                operations.as_ptr()
            },
            operation_count: operations.len(),
        };
        // SAFETY: this method has the same caller contract as apply_batch_abi.
        unsafe { self.apply_batch_abi(surface, &batch) }
    }

    /// Captures the committed canonical semantic state and releases the
    /// runtime-owned result exactly once.
    pub fn capture_semantic_state(
        &mut self,
        surface: AbiSurfaceHandle,
    ) -> Result<Vec<u8>, RuntimeSessionError> {
        let capture = self.api().capture_semantic_state;
        self.capture_owned(surface, capture)
    }

    /// Captures runtime-owned opaque surface state and releases the result
    /// exactly once.
    pub fn capture_opaque_state(
        &mut self,
        surface: AbiSurfaceHandle,
    ) -> Result<Vec<u8>, RuntimeSessionError> {
        let capture = self.api().capture_opaque_state;
        self.capture_owned(surface, capture)
    }

    /// Returns and clears all callback event snapshots currently queued.
    pub fn events(&mut self) -> Vec<RuntimeEventSnapshot> {
        self.callback_state
            .as_mut()
            .map_or_else(Vec::new, |state| state.drain_events())
    }

    /// Alias for [`RuntimeSession::events`].
    pub fn drain_events(&mut self) -> Vec<RuntimeEventSnapshot> {
        self.events()
    }
    fn capture_owned(
        &mut self,
        surface: AbiSurfaceHandle,
        capture: CaptureSemanticStateFn,
    ) -> Result<Vec<u8>, RuntimeSessionError> {
        let runtime = self.live_runtime()?;
        let mut output = empty_owned_bytes();
        // SAFETY: the output points to a Rust-owned ABI result slot and the
        // runtime writes only the slot during this call.
        let status = unsafe { capture(runtime, surface, &mut output) };
        let bytes = copy_and_release_owned_bytes(output);
        match RuntimeSessionError::from_status(status) {
            Ok(()) => bytes,
            Err(error) => {
                // A malformed provider may return bytes with a failed status;
                // still release those bytes once before returning the
                // redacted status category.
                let _ = bytes;
                Err(error)
            }
        }
    }

    /// Requests terminal shutdown and destroys the runtime exactly once.
    ///
    /// The provider's successful shutdown return is the terminal-state
    /// evidence required before calling `destroy`.  A failed shutdown is
    /// retryable and never calls `destroy`.
    pub fn shutdown(&mut self) -> Result<(), RuntimeSessionError> {
        if self.destroyed || self.runtime == 0 {
            return Ok(());
        }
        if !self.terminal {
            let request_shutdown = self.api().request_shutdown;
            // SAFETY: the handle belongs to this live session and shutdown
            // carries no borrowed pointers.
            let status = unsafe { request_shutdown(self.runtime) };
            RuntimeSessionError::from_status(status)?;
            self.terminal = true;
        }
        self.destroy_after_terminal();
        Ok(())
    }

    fn live_runtime(&self) -> Result<AbiRuntimeHandle, RuntimeSessionError> {
        if self.destroyed || self.runtime == 0 {
            Err(RuntimeSessionError::Destroyed)
        } else {
            Ok(self.runtime)
        }
    }

    fn api(&self) -> &RuntimeApi {
        // The option is only cleared after a failed Drop shutdown, where the
        // whole owner is intentionally leaked and no method can run again.
        self.library
            .as_ref()
            .expect("live runtime session retains its runtime library")
            .api()
    }

    fn destroy_after_terminal(&mut self) {
        if self.runtime == 0 || !self.terminal || self.destroyed {
            return;
        }
        let destroy = self.api().destroy;
        let runtime = self.runtime;
        // SAFETY: `terminal` is set only after a successful shutdown status,
        // and the library remains alive through this call.
        unsafe { destroy(runtime) };
        self.runtime = 0;
        self.destroyed = true;
    }

    fn leak_live_runtime(&mut self) {
        // Unloading the library or freeing callback state/API memory while a
        // provider is not terminal would make its copied function pointers,
        // callback context, or client table dangling.  Leak all three
        // together rather than violating the ABI's destroy-after-terminal
        // rule.
        if let Some(library) = self.library.take() {
            std::mem::forget(library);
        }
        if let Some(callback_state) = self.callback_state.take() {
            std::mem::forget(callback_state);
        }
        if let Some(client_api) = self.client_api.take() {
            std::mem::forget(client_api);
        }
    }
}

impl Drop for RuntimeSession {
    fn drop(&mut self) {
        if self.runtime == 0 || self.destroyed {
            return;
        }
        let shutdown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.shutdown()));
        if shutdown.is_err() || self.runtime != 0 {
            // Never call destroy on an unproven non-terminal runtime and
            // never unload its library/context in that case.
            self.leak_live_runtime();
        }
    }
}

fn runtime_string_view(value: &str, maximum: usize) -> Result<AbiStringView, RuntimeSessionError> {
    if value.len() > maximum {
        return Err(RuntimeSessionError::InvalidConfiguration);
    }
    Ok(AbiStringView {
        data: if value.is_empty() {
            std::ptr::null()
        } else {
            value.as_ptr().cast::<c_char>()
        },
        len: value.len(),
    })
}

fn runtime_bytes_view(value: &[u8], maximum: usize) -> Result<AbiBytesView, RuntimeSessionError> {
    if value.len() > maximum {
        return Err(RuntimeSessionError::InvalidArgument);
    }
    Ok(AbiBytesView {
        data: if value.is_empty() {
            std::ptr::null()
        } else {
            value.as_ptr()
        },
        len: value.len(),
    })
}

struct PreparedUiBatch<'a> {
    batch: AbiUiBatch,
    _operations: Vec<AbiUiOperation>,
    _source: std::marker::PhantomData<&'a RuntimeUiBatch>,
}

fn prepare_ui_batch(batch: &RuntimeUiBatch) -> Result<PreparedUiBatch<'_>, RuntimeSessionError> {
    if batch.operations.len() > CLIENT_MAX_RUNTIME_BATCH_OPERATIONS {
        return Err(RuntimeSessionError::InvalidArgument);
    }
    let mut operations = Vec::with_capacity(batch.operations.len());
    for operation in &batch.operations {
        operations.push(lower_ui_operation(operation)?);
    }
    let batch_view = AbiUiBatch {
        semantic_revision: batch.semantic_revision,
        operations: if operations.is_empty() {
            std::ptr::null()
        } else {
            operations.as_ptr()
        },
        operation_count: operations.len(),
    };
    Ok(PreparedUiBatch {
        batch: batch_view,
        _operations: operations,
        _source: std::marker::PhantomData,
    })
}

fn lower_string_view(value: &str) -> Result<AbiStringView, RuntimeSessionError> {
    runtime_string_view(value, CLIENT_MAX_RUNTIME_TEXT_BYTES)
        .map_err(|_| RuntimeSessionError::InvalidArgument)
}

fn lower_bytes_view(value: &[u8]) -> Result<AbiBytesView, RuntimeSessionError> {
    runtime_bytes_view(value, CLIENT_MAX_RUNTIME_VALUE_BYTES)
        .map_err(|_| RuntimeSessionError::InvalidArgument)
}

fn lower_value_input(value: &RuntimeValueInput) -> Result<AbiValueRef, RuntimeSessionError> {
    Ok(AbiValueRef {
        handle: value.handle,
        type_name: lower_string_view(&value.type_name)?,
        canonical_encoding: lower_bytes_view(&value.canonical_encoding)?,
    })
}

fn empty_value_ref() -> AbiValueRef {
    AbiValueRef {
        handle: 0,
        type_name: AbiStringView {
            data: std::ptr::null(),
            len: 0,
        },
        canonical_encoding: AbiBytesView {
            data: std::ptr::null(),
            len: 0,
        },
    }
}

fn lower_ui_operation(
    operation: &RuntimeUiOperation,
) -> Result<AbiUiOperation, RuntimeSessionError> {
    match operation {
        RuntimeUiOperation::MountNode {
            node,
            parent,
            slot,
            ordinal,
            contract_name,
            contract_major,
            contract_minor,
            explicit_key,
        } => Ok(AbiUiOperation {
            kind: AbiUiOperationKind::MOUNT_NODE,
            as_: AbiUiOperationArgs {
                mount_node: AbiMountNode {
                    node: *node,
                    parent: *parent,
                    slot: lower_string_view(slot)?,
                    ordinal: *ordinal,
                    contract_name: lower_string_view(contract_name)?,
                    contract_major: *contract_major,
                    contract_minor: *contract_minor,
                    explicit_key: lower_value_input(explicit_key)?,
                },
            },
        }),
        RuntimeUiOperation::UnmountNode { node } => Ok(AbiUiOperation {
            kind: AbiUiOperationKind::UNMOUNT_NODE,
            as_: AbiUiOperationArgs {
                unmount_node: *node,
            },
        }),
        RuntimeUiOperation::SetProperty {
            node,
            property,
            value,
        } => Ok(AbiUiOperation {
            kind: AbiUiOperationKind::SET_PROPERTY,
            as_: AbiUiOperationArgs {
                set_property: AbiSetProperty {
                    node: *node,
                    property: lower_string_view(property)?,
                    value: lower_value_input(value)?,
                },
            },
        }),
        RuntimeUiOperation::ClearProperty { node, property } => Ok(AbiUiOperation {
            kind: AbiUiOperationKind::CLEAR_PROPERTY,
            as_: AbiUiOperationArgs {
                set_property: AbiSetProperty {
                    node: *node,
                    property: lower_string_view(property)?,
                    value: empty_value_ref(),
                },
            },
        }),
        RuntimeUiOperation::InsertChild {
            parent,
            slot,
            child,
            ordinal,
        } => Ok(AbiUiOperation {
            kind: AbiUiOperationKind::INSERT_CHILD,
            as_: AbiUiOperationArgs {
                child: AbiChildOperation {
                    parent: *parent,
                    slot: lower_string_view(slot)?,
                    child: *child,
                    ordinal: *ordinal,
                },
            },
        }),
        RuntimeUiOperation::RemoveChild {
            parent,
            slot,
            child,
            ordinal,
        } => Ok(AbiUiOperation {
            kind: AbiUiOperationKind::REMOVE_CHILD,
            as_: AbiUiOperationArgs {
                child: AbiChildOperation {
                    parent: *parent,
                    slot: lower_string_view(slot)?,
                    child: *child,
                    ordinal: *ordinal,
                },
            },
        }),
        RuntimeUiOperation::MoveChild {
            parent,
            slot,
            child,
            ordinal,
        } => Ok(AbiUiOperation {
            kind: AbiUiOperationKind::MOVE_CHILD,
            as_: AbiUiOperationArgs {
                child: AbiChildOperation {
                    parent: *parent,
                    slot: lower_string_view(slot)?,
                    child: *child,
                    ordinal: *ordinal,
                },
            },
        }),
        RuntimeUiOperation::BindAction {
            node,
            event_name,
            action,
            input_type,
        } => Ok(AbiUiOperation {
            kind: AbiUiOperationKind::BIND_ACTION,
            as_: AbiUiOperationArgs {
                bind_action: AbiBindAction {
                    node: *node,
                    event_name: lower_string_view(event_name)?,
                    action: *action,
                    input_type: lower_string_view(input_type)?,
                },
            },
        }),
        RuntimeUiOperation::UnbindAction {
            node,
            event_name,
            action,
        } => Ok(AbiUiOperation {
            kind: AbiUiOperationKind::UNBIND_ACTION,
            as_: AbiUiOperationArgs {
                bind_action: AbiBindAction {
                    node: *node,
                    event_name: lower_string_view(event_name)?,
                    action: *action,
                    input_type: AbiStringView {
                        data: std::ptr::null(),
                        len: 0,
                    },
                },
            },
        }),
        RuntimeUiOperation::SetFocus | RuntimeUiOperation::SetAccessibility => {
            Err(RuntimeSessionError::Unsupported)
        }
    }
}

fn validate_batch_view(batch: &AbiUiBatch) -> Result<(), RuntimeSessionError> {
    if batch.operation_count > CLIENT_MAX_RUNTIME_BATCH_OPERATIONS {
        return Err(RuntimeSessionError::InvalidArgument);
    }
    if batch.operation_count == 0 {
        return Ok(());
    }
    let operations = batch.operations;
    if operations.is_null() || !operations.is_aligned() {
        return Err(RuntimeSessionError::InvalidArgument);
    }
    let bytes = batch
        .operation_count
        .checked_mul(std::mem::size_of::<AbiUiOperation>())
        .filter(|length| *length <= isize::MAX as usize)
        .ok_or(RuntimeSessionError::InvalidArgument)?;
    (operations as usize)
        .checked_add(bytes)
        .ok_or(RuntimeSessionError::InvalidArgument)?;
    Ok(())
}

fn callback_status(code: AbiStatusCode) -> AbiStatus {
    AbiStatus {
        code,
        message: AbiStringView {
            data: std::ptr::null(),
            len: 0,
        },
    }
}

fn empty_owned_bytes() -> AbiOwnedBytes {
    AbiOwnedBytes {
        data: std::ptr::null_mut(),
        len: 0,
        owner: std::ptr::null_mut(),
        release: release_owned_bytes_noop,
    }
}

unsafe extern "C" fn release_owned_bytes_noop(_owner: *mut c_void, _data: *mut u8, _len: usize) {}

fn copy_and_release_owned_bytes(output: AbiOwnedBytes) -> Result<Vec<u8>, RuntimeSessionError> {
    if (output.release as usize) == 0 {
        return Err(RuntimeSessionError::MalformedOwnedBytes);
    }
    let valid = output.len <= CLIENT_MAX_RUNTIME_VALUE_BYTES
        && (output.len == 0 || !output.data.is_null())
        && output
            .len
            .checked_mul(std::mem::size_of::<u8>())
            .filter(|length| *length <= isize::MAX as usize)
            .and_then(|length| (output.data as usize).checked_add(length))
            .is_some();
    let bytes = if !valid {
        None
    } else if output.len == 0 {
        Some(Vec::new())
    } else {
        // SAFETY: the descriptor has a bounded length and a non-null pointer
        // for non-empty data.  The runtime owns the allocation only until its
        // supplied release callback below.
        Some(unsafe { slice::from_raw_parts(output.data.cast::<u8>(), output.len).to_vec() })
    };
    let release_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the release function is supplied by the runtime and was
        // checked for a non-null function address.  This is the sole release
        // call for this owned-byte descriptor.
        unsafe { (output.release)(output.owner, output.data, output.len) };
    }));
    if release_result.is_err() {
        return Err(RuntimeSessionError::Internal);
    }
    bytes.ok_or(RuntimeSessionError::MalformedOwnedBytes)
}

fn copy_bounded_string(view: AbiStringView, maximum: usize) -> Result<String, AbiStatusCode> {
    if view.len > maximum {
        return Err(AbiStatusCode::INVALID_ARGUMENT);
    }
    if view.len == 0 {
        return Ok(String::new());
    }
    if view.data.is_null() || !view.data.is_aligned() {
        return Err(AbiStatusCode::INVALID_ARGUMENT);
    }
    view.len
        .checked_mul(std::mem::size_of::<c_char>())
        .filter(|length| *length <= isize::MAX as usize)
        .and_then(|length| (view.data as usize).checked_add(length))
        .ok_or(AbiStatusCode::INVALID_ARGUMENT)?;
    // SAFETY: the view has a bounded, checked address range and is copied
    // before the callback returns.
    let bytes = unsafe { slice::from_raw_parts(view.data.cast::<u8>(), view.len) };
    str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| AbiStatusCode::INVALID_ARGUMENT)
}

fn copy_bounded_bytes(view: AbiBytesView, maximum: usize) -> Result<Vec<u8>, AbiStatusCode> {
    if view.len > maximum {
        return Err(AbiStatusCode::INVALID_ARGUMENT);
    }
    if view.len == 0 {
        return Ok(Vec::new());
    }
    if view.data.is_null() {
        return Err(AbiStatusCode::INVALID_ARGUMENT);
    }
    view.len
        .checked_mul(std::mem::size_of::<u8>())
        .filter(|length| *length <= isize::MAX as usize)
        .and_then(|length| (view.data as usize).checked_add(length))
        .ok_or(AbiStatusCode::INVALID_ARGUMENT)?;
    // SAFETY: the view has a bounded, checked address range and is copied
    // before the callback returns.
    Ok(unsafe { slice::from_raw_parts(view.data, view.len).to_vec() })
}

fn copy_value_snapshot(value: AbiValueRef) -> Result<RuntimeValueSnapshot, AbiStatusCode> {
    Ok(RuntimeValueSnapshot {
        handle: value.handle,
        type_name: copy_bounded_string(value.type_name, CLIENT_MAX_RUNTIME_TEXT_BYTES)?,
        canonical_encoding: copy_bounded_bytes(
            value.canonical_encoding,
            CLIENT_MAX_RUNTIME_VALUE_BYTES,
        )?,
    })
}

fn copy_runtime_event(event: &AbiRuntimeEvent) -> Result<RuntimeEventSnapshot, AbiStatusCode> {
    match event.kind {
        kind if kind == AbiEventKind::ACTION => {
            // SAFETY: the union member is selected by the event kind.
            let action = unsafe { event.as_.action };
            Ok(RuntimeEventSnapshot::Action(RuntimeActionEventSnapshot {
                surface: action.surface,
                node: action.node,
                action: action.action,
                payload: copy_value_snapshot(action.payload)?,
            }))
        }
        kind if kind == AbiEventKind::SURFACE_CLOSED => {
            // SAFETY: the union member is selected by the event kind.
            let surface = unsafe { event.as_.surface_closed };
            Ok(RuntimeEventSnapshot::SurfaceClosed(
                RuntimeSurfaceClosedEventSnapshot {
                    surface: surface.surface,
                },
            ))
        }
        kind if kind == AbiEventKind::DIAGNOSTIC => {
            // SAFETY: the union member is selected by the event kind.
            let diagnostic = unsafe { event.as_.diagnostic };
            Ok(RuntimeEventSnapshot::Diagnostic(
                RuntimeDiagnosticEventSnapshot {
                    code: diagnostic.status.code,
                    message: copy_bounded_string(
                        diagnostic.status.message,
                        CLIENT_MAX_RUNTIME_TEXT_BYTES,
                    )?,
                },
            ))
        }
        _ => Err(AbiStatusCode::UNSUPPORTED),
    }
}

unsafe fn callback_state_mut<'a>(
    context: *mut c_void,
) -> Result<&'a mut CallbackState, AbiStatusCode> {
    if context.is_null() || !context.cast::<CallbackState>().is_aligned() {
        return Err(AbiStatusCode::INVALID_ARGUMENT);
    }
    // SAFETY: `context` is installed from a live boxed CallbackState by
    // RuntimeSession::new_qt.  The caller-pumps ABI serializes callbacks and
    // session operations, so no second mutable reference is created here.
    Ok(unsafe { &mut *context.cast::<CallbackState>() })
}

unsafe fn emit_runtime_event_inner(
    context: *mut c_void,
    runtime: AbiRuntimeHandle,
    event: *const AbiRuntimeEvent,
) -> Result<(), AbiStatusCode> {
    let state = unsafe { callback_state_mut(context)? };
    if event.is_null() || !event.is_aligned() {
        return Err(AbiStatusCode::INVALID_ARGUMENT);
    }
    validate_native_range(event, std::mem::size_of::<AbiRuntimeEvent>())
        .map_err(|_| AbiStatusCode::INVALID_ARGUMENT)?;
    if state.runtime != 0 && state.runtime != runtime {
        return Err(AbiStatusCode::INVALID_ARGUMENT);
    }
    // SAFETY: null/alignment were checked above; the provider owns the event
    // only for this callback and copy_runtime_event does not retain it.
    let snapshot = unsafe { copy_runtime_event(&*event)? };
    state.push_event(snapshot)
}

unsafe extern "C" fn callback_emit_runtime_event(
    context: *mut c_void,
    runtime: AbiRuntimeHandle,
    event: *const AbiRuntimeEvent,
) -> AbiStatus {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the inner callback validates the opaque context/event
        // pointers before dereferencing either.
        unsafe { emit_runtime_event_inner(context, runtime, event) }
    }));
    match result {
        Ok(Ok(())) => callback_status(AbiStatusCode::OK),
        Ok(Err(code)) => callback_status(code),
        Err(_) => callback_status(AbiStatusCode::INTERNAL),
    }
}

unsafe extern "C" fn callback_log(
    _context: *mut c_void,
    _level: u32,
    _subsystem: AbiStringView,
    _message: AbiStringView,
) {
    // Logging is intentionally not retained by this session boundary.  The
    // provider's redacted status/event channel is the supported output.
}

unsafe extern "C" fn callback_complete_model_request(
    context: *mut c_void,
    _request: AbiRequestHandle,
    _result: AbiValueRef,
) -> AbiStatus {
    if context.is_null() {
        callback_status(AbiStatusCode::INVALID_ARGUMENT)
    } else {
        callback_status(AbiStatusCode::UNSUPPORTED)
    }
}

unsafe extern "C" fn callback_fail_model_request(
    context: *mut c_void,
    _request: AbiRequestHandle,
    _failure: AbiStatus,
) -> AbiStatus {
    if context.is_null() {
        callback_status(AbiStatusCode::INVALID_ARGUMENT)
    } else {
        callback_status(AbiStatusCode::UNSUPPORTED)
    }
}

unsafe extern "C" fn callback_read_action_metadata(
    context: *mut c_void,
    _action: AbiActionHandle,
    output: *mut AbiOwnedBytes,
) -> AbiStatus {
    if context.is_null() || output.is_null() || !output.is_aligned() {
        return callback_status(AbiStatusCode::INVALID_ARGUMENT);
    }
    // SAFETY: null/alignment were checked above; no native bytes are
    // available from this session boundary, so return a valid empty result.
    unsafe { *output = empty_owned_bytes() };
    callback_status(AbiStatusCode::UNSUPPORTED)
}

unsafe extern "C" fn callback_read_value_debug_json(
    context: *mut c_void,
    _value: AbiValueRef,
    output: *mut AbiOwnedBytes,
) -> AbiStatus {
    if context.is_null() || output.is_null() || !output.is_aligned() {
        return callback_status(AbiStatusCode::INVALID_ARGUMENT);
    }
    // SAFETY: null/alignment were checked above; no native bytes are
    // available from this session boundary, so return a valid empty result.
    unsafe { *output = empty_owned_bytes() };
    callback_status(AbiStatusCode::UNSUPPORTED)
}

unsafe extern "C" fn callback_monotonic_time_ns(context: *mut c_void) -> u64 {
    if context.is_null() {
        return 0;
    }
    static START: std::sync::LazyLock<std::time::Instant> =
        std::sync::LazyLock::new(std::time::Instant::now);
    let elapsed = START.elapsed().as_nanos();
    elapsed.min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static RELEASE_COUNT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_release(_owner: *mut c_void, data: *mut u8, len: usize) {
        RELEASE_COUNT.fetch_add(1, Ordering::SeqCst);
        if !data.is_null() {
            // SAFETY: the test allocates this exact Vec immediately below.
            unsafe { drop(Vec::from_raw_parts(data, len, len)) };
        }
    }

    #[test]
    fn owned_bytes_are_copied_and_released_once() {
        RELEASE_COUNT.store(0, Ordering::SeqCst);
        let mut bytes = b"semantic".to_vec();
        let output = AbiOwnedBytes {
            data: bytes.as_mut_ptr(),
            len: bytes.len(),
            owner: std::ptr::null_mut(),
            release: count_release,
        };
        std::mem::forget(bytes);

        let copied = copy_and_release_owned_bytes(output).expect("valid owned bytes");
        assert_eq!(copied, b"semantic");
        assert_eq!(RELEASE_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn action_event_is_owned_before_callback_returns() {
        let type_name = b"std.text";
        let payload = b"payload";
        let mut state = CallbackState::new();
        state.runtime = 7;
        let event = AbiRuntimeEvent {
            kind: AbiEventKind::ACTION,
            as_: AbiRuntimeEventArgs {
                action: AbiActionEvent {
                    surface: 1,
                    node: 2,
                    action: 3,
                    payload: AbiValueRef {
                        handle: 4,
                        type_name: AbiStringView {
                            data: type_name.as_ptr().cast(),
                            len: type_name.len(),
                        },
                        canonical_encoding: AbiBytesView {
                            data: payload.as_ptr(),
                            len: payload.len(),
                        },
                    },
                },
            },
        };
        let context = (&mut state as *mut CallbackState).cast::<c_void>();
        assert_eq!(
            unsafe { callback_emit_runtime_event(context, 7, &event) }.code,
            AbiStatusCode::OK
        );
        let snapshots = state.drain_events();
        assert_eq!(
            snapshots,
            vec![RuntimeEventSnapshot::Action(RuntimeActionEventSnapshot {
                surface: 1,
                node: 2,
                action: 3,
                payload: RuntimeValueSnapshot {
                    handle: 4,
                    type_name: "std.text".to_owned(),
                    canonical_encoding: b"payload".to_vec(),
                },
            })]
        );
    }

    #[test]
    fn malformed_event_view_is_rejected_without_pointer_escape() {
        let mut state = CallbackState::new();
        state.runtime = 9;
        let event = AbiRuntimeEvent {
            kind: AbiEventKind::DIAGNOSTIC,
            as_: AbiRuntimeEventArgs {
                diagnostic: AbiDiagnosticEvent {
                    status: AbiStatus {
                        code: AbiStatusCode::FAILED,
                        message: AbiStringView {
                            data: std::ptr::null(),
                            len: 1,
                        },
                    },
                },
            },
        };
        let context = (&mut state as *mut CallbackState).cast::<c_void>();
        assert_eq!(
            unsafe { callback_emit_runtime_event(context, 9, &event) }.code,
            AbiStatusCode::INVALID_ARGUMENT
        );
        assert!(state.events.is_empty());
    }
}
