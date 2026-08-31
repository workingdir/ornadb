use std::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct StringView {
    pub(super) data: *const c_char,
    pub(super) len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct BytesView {
    pub(super) data: *const u8,
    pub(super) len: usize,
}

pub(super) type ReleaseFn = unsafe extern "C" fn(*mut c_void, *mut u8, usize);

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct OwnedBytes {
    pub(super) data: *mut u8,
    pub(super) len: usize,
    pub(super) owner: *mut c_void,
    pub(super) release: ReleaseFn,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StatusCode(pub(super) i32);

#[allow(dead_code, non_upper_case_globals)]
impl StatusCode {
    pub(super) const Ok: Self = Self(0);
    pub(super) const InvalidArgument: Self = Self(1);
    pub(super) const Unsupported: Self = Self(2);
    pub(super) const NotFound: Self = Self(3);
    pub(super) const Busy: Self = Self(4);
    pub(super) const Cancelled: Self = Self(5);
    pub(super) const Failed: Self = Self(6);
    pub(super) const Internal: Self = Self(7);
    pub(super) const StaleRevision: Self = Self(8);
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct Status {
    pub(super) code: StatusCode,
    pub(super) message: StringView,
}

pub(super) type Handle = u64;
pub(super) type RuntimeHandle = Handle;
pub(super) type SurfaceHandle = Handle;
pub(super) type NodeHandle = Handle;
pub(super) type ActionHandle = Handle;
pub(super) type ModelHandle = Handle;
pub(super) type RequestHandle = Handle;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SurfaceClosedEvent {
    pub(super) surface: SurfaceHandle,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct DiagnosticEvent {
    pub(super) status: Status,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ThreadModel(pub(super) i32);

#[allow(dead_code, non_upper_case_globals)]
impl ThreadModel {
    pub(super) const ClientEventLoop: Self = Self(1);
    pub(super) const RuntimeEventLoop: Self = Self(2);
    pub(super) const CallerPumps: Self = Self(3);
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RuntimeFeature(pub(super) i32);

#[allow(dead_code, non_upper_case_globals)]
impl RuntimeFeature {
    pub(super) const MultipleWindows: Self = Self(1 << 0);
    pub(super) const Accessibility: Self = Self(1 << 1);
    pub(super) const Clipboard: Self = Self(1 << 2);
    pub(super) const DragDrop: Self = Self(1 << 3);
    pub(super) const NativeMenus: Self = Self(1 << 4);
    pub(super) const Printing: Self = Self(1 << 5);
    pub(super) const OpaqueLayoutState: Self = Self(1 << 6);
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ContractVersion {
    pub(super) name: StringView,
    pub(super) major: u32,
    pub(super) minor: u32,
    pub(super) features: *const StringView,
    pub(super) feature_count: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SinkOffer {
    pub(super) type_name: StringView,
    pub(super) media_types: *const StringView,
    pub(super) media_type_count: usize,
    pub(super) supports_streaming: u8,
    pub(super) preference_rank: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct Descriptor {
    pub(super) abi_major: u32,
    pub(super) abi_minor: u32,
    pub(super) runtime_name: StringView,
    pub(super) runtime_version: StringView,
    pub(super) build_id: StringView,
    pub(super) platform: StringView,
    pub(super) thread_model: ThreadModel,
    pub(super) features: u64,
    pub(super) sinks: *const SinkOffer,
    pub(super) sink_count: usize,
    pub(super) contracts: *const ContractVersion,
    pub(super) contract_count: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ValueRef {
    pub(super) handle: Handle,
    pub(super) type_name: StringView,
    pub(super) canonical_encoding: BytesView,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct UiOperationKind(pub(super) i32);

#[allow(non_upper_case_globals)]
impl UiOperationKind {
    pub(super) const MountNode: Self = Self(1);
    pub(super) const UnmountNode: Self = Self(2);
    pub(super) const SetProperty: Self = Self(3);
    pub(super) const ClearProperty: Self = Self(4);
    pub(super) const InsertChild: Self = Self(5);
    pub(super) const RemoveChild: Self = Self(6);
    pub(super) const MoveChild: Self = Self(7);
    pub(super) const BindAction: Self = Self(8);
    pub(super) const UnbindAction: Self = Self(9);
    pub(super) const SetFocus: Self = Self(10);
    pub(super) const SetAccessibility: Self = Self(11);
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct MountNode {
    pub(super) node: NodeHandle,
    pub(super) parent: NodeHandle,
    pub(super) slot: StringView,
    pub(super) ordinal: usize,
    pub(super) contract_name: StringView,
    pub(super) contract_major: u32,
    pub(super) contract_minor: u32,
    pub(super) explicit_key: ValueRef,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SetProperty {
    pub(super) node: NodeHandle,
    pub(super) property: StringView,
    pub(super) value: ValueRef,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ChildOperation {
    pub(super) parent: NodeHandle,
    pub(super) slot: StringView,
    pub(super) child: NodeHandle,
    pub(super) ordinal: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct BindAction {
    pub(super) node: NodeHandle,
    pub(super) event_name: StringView,
    pub(super) action: ActionHandle,
    pub(super) input_type: StringView,
}

#[repr(C)]
pub(super) union UiOperationArgs {
    pub(super) mount_node: MountNode,
    pub(super) unmount_node: NodeHandle,
    pub(super) set_property: SetProperty,
    pub(super) child: ChildOperation,
    pub(super) bind_action: BindAction,
}

#[repr(C)]
pub(super) struct UiOperation {
    pub(super) kind: UiOperationKind,
    pub(super) as_: UiOperationArgs,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct UiBatch {
    pub(super) semantic_revision: u64,
    pub(super) operations: *const UiOperation,
    pub(super) operation_count: usize,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EventKind(pub(super) i32);

#[allow(non_upper_case_globals)]
impl EventKind {
    pub(super) const Action: Self = Self(1);
    pub(super) const FocusChanged: Self = Self(2);
    pub(super) const LayoutStateChanged: Self = Self(3);
    pub(super) const SurfaceClosed: Self = Self(4);
    pub(super) const ModelRangeRequest: Self = Self(5);
    pub(super) const ModelChildrenRequest: Self = Self(6);
    pub(super) const Diagnostic: Self = Self(7);
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ActionEvent {
    pub(super) surface: SurfaceHandle,
    pub(super) node: NodeHandle,
    pub(super) action: ActionHandle,
    pub(super) payload: ValueRef,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct LayoutStateEvent {
    pub(super) surface: SurfaceHandle,
    pub(super) node: NodeHandle,
    pub(super) semantic_state_name: StringView,
    pub(super) semantic_state: ValueRef,
    pub(super) opaque_runtime_state: BytesView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ModelRangeRequest {
    pub(super) request: RequestHandle,
    pub(super) model: ModelHandle,
    pub(super) start: u64,
    pub(super) count: u64,
    pub(super) sort_filter_token: StringView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ModelChildrenRequest {
    pub(super) request: RequestHandle,
    pub(super) model: ModelHandle,
    pub(super) parent_key: ValueRef,
}

#[repr(C)]
pub(super) union RuntimeEventArgs {
    pub(super) action: ActionEvent,
    pub(super) layout_state: LayoutStateEvent,
    pub(super) range_request: ModelRangeRequest,
    pub(super) children_request: ModelChildrenRequest,
    pub(super) surface_closed: SurfaceClosedEvent,
    pub(super) diagnostic: DiagnosticEvent,
}

#[repr(C)]
pub(super) struct RuntimeEvent {
    pub(super) kind: EventKind,
    pub(super) as_: RuntimeEventArgs,
}

pub(super) type LogFn = unsafe extern "C" fn(*mut c_void, u32, StringView, StringView);
pub(super) type EmitRuntimeEventFn =
    unsafe extern "C" fn(*mut c_void, RuntimeHandle, *const RuntimeEvent) -> Status;
pub(super) type CompleteModelRequestFn =
    unsafe extern "C" fn(*mut c_void, RequestHandle, ValueRef) -> Status;
pub(super) type FailModelRequestFn =
    unsafe extern "C" fn(*mut c_void, RequestHandle, Status) -> Status;
pub(super) type ReadActionMetadataFn =
    unsafe extern "C" fn(*mut c_void, ActionHandle, *mut OwnedBytes) -> Status;
pub(super) type ReadValueDebugJsonFn =
    unsafe extern "C" fn(*mut c_void, ValueRef, *mut OwnedBytes) -> Status;
pub(super) type MonotonicTimeFn = unsafe extern "C" fn(*mut c_void) -> u64;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ClientApi {
    pub(super) abi_major: u32,
    pub(super) abi_minor: u32,
    pub(super) context: *mut c_void,
    pub(super) log: LogFn,
    pub(super) emit_runtime_event: EmitRuntimeEventFn,
    pub(super) complete_model_request: CompleteModelRequestFn,
    pub(super) fail_model_request: FailModelRequestFn,
    pub(super) read_action_metadata: ReadActionMetadataFn,
    pub(super) read_value_debug_json: ReadValueDebugJsonFn,
    pub(super) monotonic_time_ns: MonotonicTimeFn,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct RuntimeCreateOptions {
    pub(super) client: *const ClientApi,
    pub(super) locale: StringView,
    pub(super) timezone: StringView,
    pub(super) theme: StringView,
    pub(super) accessibility_preferences_json: StringView,
    pub(super) runtime_configuration_json: StringView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SurfaceCreateOptions {
    pub(super) surface_kind: StringView,
    pub(super) title: StringView,
    pub(super) state_profile: StringView,
    pub(super) opaque_runtime_restore_state: BytesView,
}

pub(super) type DescribeFn = unsafe extern "C" fn() -> *const Descriptor;
pub(super) type CreateFn =
    unsafe extern "C" fn(*const RuntimeCreateOptions, *mut RuntimeHandle) -> Status;
pub(super) type DestroyFn = unsafe extern "C" fn(RuntimeHandle);
pub(super) type StartEventLoopFn = unsafe extern "C" fn(RuntimeHandle) -> Status;
pub(super) type PollEventLoopFn = unsafe extern "C" fn(RuntimeHandle, u32) -> Status;
pub(super) type RequestShutdownFn = unsafe extern "C" fn(RuntimeHandle) -> Status;
pub(super) type CreateSurfaceFn =
    unsafe extern "C" fn(RuntimeHandle, *const SurfaceCreateOptions, *mut SurfaceHandle) -> Status;
pub(super) type DestroySurfaceFn = unsafe extern "C" fn(RuntimeHandle, SurfaceHandle) -> Status;
pub(super) type ApplyUiBatchFn =
    unsafe extern "C" fn(RuntimeHandle, SurfaceHandle, *const UiBatch) -> Status;
pub(super) type SetSurfaceVisibleFn =
    unsafe extern "C" fn(RuntimeHandle, SurfaceHandle, u8) -> Status;
pub(super) type CaptureSemanticStateFn =
    unsafe extern "C" fn(RuntimeHandle, SurfaceHandle, *mut OwnedBytes) -> Status;
pub(super) type CaptureOpaqueStateFn =
    unsafe extern "C" fn(RuntimeHandle, SurfaceHandle, *mut OwnedBytes) -> Status;
pub(super) type ApplyModelRowsFn =
    unsafe extern "C" fn(RuntimeHandle, RequestHandle, ValueRef) -> Status;
pub(super) type CancelRequestFn = unsafe extern "C" fn(RuntimeHandle, RequestHandle) -> Status;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct RuntimeApi {
    pub(super) abi_major: u32,
    pub(super) abi_minor: u32,
    pub(super) describe: DescribeFn,
    pub(super) create: CreateFn,
    pub(super) destroy: DestroyFn,
    pub(super) start_event_loop: StartEventLoopFn,
    pub(super) poll_event_loop: PollEventLoopFn,
    pub(super) request_shutdown: RequestShutdownFn,
    pub(super) create_surface: CreateSurfaceFn,
    pub(super) destroy_surface: DestroySurfaceFn,
    pub(super) apply_ui_batch: ApplyUiBatchFn,
    pub(super) set_surface_visible: SetSurfaceVisibleFn,
    pub(super) capture_semantic_state: CaptureSemanticStateFn,
    pub(super) capture_opaque_state: CaptureOpaqueStateFn,
    pub(super) apply_model_rows: ApplyModelRowsFn,
    pub(super) cancel_request: CancelRequestFn,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const _: () = {
    assert!(std::mem::size_of::<Handle>() == 8);
    assert!(std::mem::align_of::<Handle>() == 8);
    assert!(std::mem::size_of::<StringView>() == 16);
    assert!(std::mem::align_of::<StringView>() == 8);
    assert!(std::mem::offset_of!(StringView, data) == 0);
    assert!(std::mem::offset_of!(StringView, len) == 8);
    assert!(std::mem::size_of::<BytesView>() == 16);
    assert!(std::mem::align_of::<BytesView>() == 8);
    assert!(std::mem::offset_of!(BytesView, data) == 0);
    assert!(std::mem::offset_of!(BytesView, len) == 8);
    assert!(std::mem::size_of::<OwnedBytes>() == 32);
    assert!(std::mem::align_of::<OwnedBytes>() == 8);
    assert!(std::mem::offset_of!(OwnedBytes, data) == 0);
    assert!(std::mem::offset_of!(OwnedBytes, len) == 8);
    assert!(std::mem::offset_of!(OwnedBytes, owner) == 16);
    assert!(std::mem::offset_of!(OwnedBytes, release) == 24);
    assert!(std::mem::size_of::<StatusCode>() == 4);
    assert!(std::mem::align_of::<StatusCode>() == 4);
    assert!(StatusCode::Ok.0 == 0);
    assert!(StatusCode::InvalidArgument.0 == 1);
    assert!(StatusCode::Unsupported.0 == 2);
    assert!(StatusCode::NotFound.0 == 3);
    assert!(StatusCode::Busy.0 == 4);
    assert!(StatusCode::Cancelled.0 == 5);
    assert!(StatusCode::Failed.0 == 6);
    assert!(StatusCode::Internal.0 == 7);
    assert!(StatusCode::StaleRevision.0 == 8);
    assert!(std::mem::size_of::<Status>() == 24);
    assert!(std::mem::align_of::<Status>() == 8);
    assert!(std::mem::offset_of!(Status, code) == 0);
    assert!(std::mem::offset_of!(Status, message) == 8);
    assert!(std::mem::size_of::<SurfaceClosedEvent>() == 8);
    assert!(std::mem::align_of::<SurfaceClosedEvent>() == 8);
    assert!(std::mem::offset_of!(SurfaceClosedEvent, surface) == 0);
    assert!(std::mem::size_of::<DiagnosticEvent>() == 24);
    assert!(std::mem::align_of::<DiagnosticEvent>() == 8);
    assert!(std::mem::offset_of!(DiagnosticEvent, status) == 0);
    assert!(std::mem::size_of::<ThreadModel>() == 4);
    assert!(std::mem::align_of::<ThreadModel>() == 4);
    assert!(ThreadModel::ClientEventLoop.0 == 1);
    assert!(ThreadModel::RuntimeEventLoop.0 == 2);
    assert!(ThreadModel::CallerPumps.0 == 3);
    assert!(std::mem::size_of::<RuntimeFeature>() == 4);
    assert!(std::mem::align_of::<RuntimeFeature>() == 4);
    assert!(RuntimeFeature::MultipleWindows.0 == (1 << 0));
    assert!(RuntimeFeature::Accessibility.0 == (1 << 1));
    assert!(RuntimeFeature::Clipboard.0 == (1 << 2));
    assert!(RuntimeFeature::DragDrop.0 == (1 << 3));
    assert!(RuntimeFeature::NativeMenus.0 == (1 << 4));
    assert!(RuntimeFeature::Printing.0 == (1 << 5));
    assert!(RuntimeFeature::OpaqueLayoutState.0 == (1 << 6));
    assert!(std::mem::size_of::<ContractVersion>() == 40);
    assert!(std::mem::align_of::<ContractVersion>() == 8);
    assert!(std::mem::offset_of!(ContractVersion, name) == 0);
    assert!(std::mem::offset_of!(ContractVersion, major) == 16);
    assert!(std::mem::offset_of!(ContractVersion, minor) == 20);
    assert!(std::mem::offset_of!(ContractVersion, features) == 24);
    assert!(std::mem::offset_of!(ContractVersion, feature_count) == 32);
    assert!(std::mem::size_of::<SinkOffer>() == 40);
    assert!(std::mem::align_of::<SinkOffer>() == 8);
    assert!(std::mem::offset_of!(SinkOffer, type_name) == 0);
    assert!(std::mem::offset_of!(SinkOffer, media_types) == 16);
    assert!(std::mem::offset_of!(SinkOffer, media_type_count) == 24);
    assert!(std::mem::offset_of!(SinkOffer, supports_streaming) == 32);
    assert!(std::mem::offset_of!(SinkOffer, preference_rank) == 36);
    assert!(std::mem::size_of::<Descriptor>() == 120);
    assert!(std::mem::align_of::<Descriptor>() == 8);
    assert!(std::mem::offset_of!(Descriptor, abi_major) == 0);
    assert!(std::mem::offset_of!(Descriptor, abi_minor) == 4);
    assert!(std::mem::offset_of!(Descriptor, runtime_name) == 8);
    assert!(std::mem::offset_of!(Descriptor, runtime_version) == 24);
    assert!(std::mem::offset_of!(Descriptor, build_id) == 40);
    assert!(std::mem::offset_of!(Descriptor, platform) == 56);
    assert!(std::mem::offset_of!(Descriptor, thread_model) == 72);
    assert!(std::mem::offset_of!(Descriptor, features) == 80);
    assert!(std::mem::offset_of!(Descriptor, sinks) == 88);
    assert!(std::mem::offset_of!(Descriptor, sink_count) == 96);
    assert!(std::mem::offset_of!(Descriptor, contracts) == 104);
    assert!(std::mem::offset_of!(Descriptor, contract_count) == 112);
    assert!(std::mem::size_of::<ValueRef>() == 40);
    assert!(std::mem::align_of::<ValueRef>() == 8);
    assert!(std::mem::offset_of!(ValueRef, handle) == 0);
    assert!(std::mem::offset_of!(ValueRef, type_name) == 8);
    assert!(std::mem::offset_of!(ValueRef, canonical_encoding) == 24);
    assert!(std::mem::size_of::<UiOperationKind>() == 4);
    assert!(std::mem::align_of::<UiOperationKind>() == 4);
    assert!(UiOperationKind::MountNode.0 == 1);
    assert!(UiOperationKind::UnmountNode.0 == 2);
    assert!(UiOperationKind::SetProperty.0 == 3);
    assert!(UiOperationKind::ClearProperty.0 == 4);
    assert!(UiOperationKind::InsertChild.0 == 5);
    assert!(UiOperationKind::RemoveChild.0 == 6);
    assert!(UiOperationKind::MoveChild.0 == 7);
    assert!(UiOperationKind::BindAction.0 == 8);
    assert!(UiOperationKind::UnbindAction.0 == 9);
    assert!(UiOperationKind::SetFocus.0 == 10);
    assert!(UiOperationKind::SetAccessibility.0 == 11);
    assert!(std::mem::size_of::<MountNode>() == 104);
    assert!(std::mem::align_of::<MountNode>() == 8);
    assert!(std::mem::offset_of!(MountNode, node) == 0);
    assert!(std::mem::offset_of!(MountNode, parent) == 8);
    assert!(std::mem::offset_of!(MountNode, slot) == 16);
    assert!(std::mem::offset_of!(MountNode, ordinal) == 32);
    assert!(std::mem::offset_of!(MountNode, contract_name) == 40);
    assert!(std::mem::offset_of!(MountNode, contract_major) == 56);
    assert!(std::mem::offset_of!(MountNode, contract_minor) == 60);
    assert!(std::mem::offset_of!(MountNode, explicit_key) == 64);
    assert!(std::mem::size_of::<SetProperty>() == 64);
    assert!(std::mem::align_of::<SetProperty>() == 8);
    assert!(std::mem::offset_of!(SetProperty, node) == 0);
    assert!(std::mem::offset_of!(SetProperty, property) == 8);
    assert!(std::mem::offset_of!(SetProperty, value) == 24);
    assert!(std::mem::size_of::<ChildOperation>() == 40);
    assert!(std::mem::align_of::<ChildOperation>() == 8);
    assert!(std::mem::offset_of!(ChildOperation, parent) == 0);
    assert!(std::mem::offset_of!(ChildOperation, slot) == 8);
    assert!(std::mem::offset_of!(ChildOperation, child) == 24);
    assert!(std::mem::offset_of!(ChildOperation, ordinal) == 32);
    assert!(std::mem::size_of::<BindAction>() == 48);
    assert!(std::mem::align_of::<BindAction>() == 8);
    assert!(std::mem::offset_of!(BindAction, node) == 0);
    assert!(std::mem::offset_of!(BindAction, event_name) == 8);
    assert!(std::mem::offset_of!(BindAction, action) == 24);
    assert!(std::mem::offset_of!(BindAction, input_type) == 32);
    assert!(std::mem::size_of::<UiOperationArgs>() == 104);
    assert!(std::mem::align_of::<UiOperationArgs>() == 8);
    assert!(std::mem::size_of::<UiOperation>() == 112);
    assert!(std::mem::align_of::<UiOperation>() == 8);
    assert!(std::mem::offset_of!(UiOperation, kind) == 0);
    assert!(std::mem::offset_of!(UiOperation, as_) == 8);
    assert!(std::mem::size_of::<UiBatch>() == 24);
    assert!(std::mem::align_of::<UiBatch>() == 8);
    assert!(std::mem::offset_of!(UiBatch, semantic_revision) == 0);
    assert!(std::mem::offset_of!(UiBatch, operations) == 8);
    assert!(std::mem::offset_of!(UiBatch, operation_count) == 16);
    assert!(std::mem::size_of::<EventKind>() == 4);
    assert!(std::mem::align_of::<EventKind>() == 4);
    assert!(EventKind::Action.0 == 1);
    assert!(EventKind::FocusChanged.0 == 2);
    assert!(EventKind::LayoutStateChanged.0 == 3);
    assert!(EventKind::SurfaceClosed.0 == 4);
    assert!(EventKind::ModelRangeRequest.0 == 5);
    assert!(EventKind::ModelChildrenRequest.0 == 6);
    assert!(EventKind::Diagnostic.0 == 7);
    assert!(std::mem::size_of::<ActionEvent>() == 64);
    assert!(std::mem::align_of::<ActionEvent>() == 8);
    assert!(std::mem::offset_of!(ActionEvent, surface) == 0);
    assert!(std::mem::offset_of!(ActionEvent, node) == 8);
    assert!(std::mem::offset_of!(ActionEvent, action) == 16);
    assert!(std::mem::offset_of!(ActionEvent, payload) == 24);
    assert!(std::mem::size_of::<LayoutStateEvent>() == 88);
    assert!(std::mem::align_of::<LayoutStateEvent>() == 8);
    assert!(std::mem::offset_of!(LayoutStateEvent, surface) == 0);
    assert!(std::mem::offset_of!(LayoutStateEvent, node) == 8);
    assert!(std::mem::offset_of!(LayoutStateEvent, semantic_state_name) == 16);
    assert!(std::mem::offset_of!(LayoutStateEvent, semantic_state) == 32);
    assert!(std::mem::offset_of!(LayoutStateEvent, opaque_runtime_state) == 72);
    assert!(std::mem::size_of::<ModelRangeRequest>() == 48);
    assert!(std::mem::align_of::<ModelRangeRequest>() == 8);
    assert!(std::mem::offset_of!(ModelRangeRequest, request) == 0);
    assert!(std::mem::offset_of!(ModelRangeRequest, model) == 8);
    assert!(std::mem::offset_of!(ModelRangeRequest, start) == 16);
    assert!(std::mem::offset_of!(ModelRangeRequest, count) == 24);
    assert!(std::mem::offset_of!(ModelRangeRequest, sort_filter_token) == 32);
    assert!(std::mem::size_of::<ModelChildrenRequest>() == 56);
    assert!(std::mem::align_of::<ModelChildrenRequest>() == 8);
    assert!(std::mem::offset_of!(ModelChildrenRequest, request) == 0);
    assert!(std::mem::offset_of!(ModelChildrenRequest, model) == 8);
    assert!(std::mem::offset_of!(ModelChildrenRequest, parent_key) == 16);
    assert!(std::mem::size_of::<RuntimeEventArgs>() == 88);
    assert!(std::mem::align_of::<RuntimeEventArgs>() == 8);
    assert!(std::mem::size_of::<RuntimeEvent>() == 96);
    assert!(std::mem::align_of::<RuntimeEvent>() == 8);
    assert!(std::mem::offset_of!(RuntimeEvent, kind) == 0);
    assert!(std::mem::offset_of!(RuntimeEvent, as_) == 8);
    assert!(std::mem::size_of::<ClientApi>() == 72);
    assert!(std::mem::align_of::<ClientApi>() == 8);
    assert!(std::mem::offset_of!(ClientApi, abi_major) == 0);
    assert!(std::mem::offset_of!(ClientApi, abi_minor) == 4);
    assert!(std::mem::offset_of!(ClientApi, context) == 8);
    assert!(std::mem::offset_of!(ClientApi, log) == 16);
    assert!(std::mem::offset_of!(ClientApi, emit_runtime_event) == 24);
    assert!(std::mem::offset_of!(ClientApi, complete_model_request) == 32);
    assert!(std::mem::offset_of!(ClientApi, fail_model_request) == 40);
    assert!(std::mem::offset_of!(ClientApi, read_action_metadata) == 48);
    assert!(std::mem::offset_of!(ClientApi, read_value_debug_json) == 56);
    assert!(std::mem::offset_of!(ClientApi, monotonic_time_ns) == 64);
    assert!(std::mem::size_of::<RuntimeCreateOptions>() == 88);
    assert!(std::mem::align_of::<RuntimeCreateOptions>() == 8);
    assert!(std::mem::offset_of!(RuntimeCreateOptions, client) == 0);
    assert!(std::mem::offset_of!(RuntimeCreateOptions, locale) == 8);
    assert!(std::mem::offset_of!(RuntimeCreateOptions, timezone) == 24);
    assert!(std::mem::offset_of!(RuntimeCreateOptions, theme) == 40);
    assert!(std::mem::offset_of!(RuntimeCreateOptions, accessibility_preferences_json) == 56);
    assert!(std::mem::offset_of!(RuntimeCreateOptions, runtime_configuration_json) == 72);
    assert!(std::mem::size_of::<SurfaceCreateOptions>() == 64);
    assert!(std::mem::align_of::<SurfaceCreateOptions>() == 8);
    assert!(std::mem::offset_of!(SurfaceCreateOptions, surface_kind) == 0);
    assert!(std::mem::offset_of!(SurfaceCreateOptions, title) == 16);
    assert!(std::mem::offset_of!(SurfaceCreateOptions, state_profile) == 32);
    assert!(std::mem::offset_of!(SurfaceCreateOptions, opaque_runtime_restore_state) == 48);
    assert!(std::mem::size_of::<RuntimeApi>() == 120);
    assert!(std::mem::align_of::<RuntimeApi>() == 8);
    assert!(std::mem::offset_of!(RuntimeApi, abi_major) == 0);
    assert!(std::mem::offset_of!(RuntimeApi, abi_minor) == 4);
    assert!(std::mem::offset_of!(RuntimeApi, describe) == 8);
    assert!(std::mem::offset_of!(RuntimeApi, create) == 16);
    assert!(std::mem::offset_of!(RuntimeApi, destroy) == 24);
    assert!(std::mem::offset_of!(RuntimeApi, start_event_loop) == 32);
    assert!(std::mem::offset_of!(RuntimeApi, poll_event_loop) == 40);
    assert!(std::mem::offset_of!(RuntimeApi, request_shutdown) == 48);
    assert!(std::mem::offset_of!(RuntimeApi, create_surface) == 56);
    assert!(std::mem::offset_of!(RuntimeApi, destroy_surface) == 64);
    assert!(std::mem::offset_of!(RuntimeApi, apply_ui_batch) == 72);
    assert!(std::mem::offset_of!(RuntimeApi, set_surface_visible) == 80);
    assert!(std::mem::offset_of!(RuntimeApi, capture_semantic_state) == 88);
    assert!(std::mem::offset_of!(RuntimeApi, capture_opaque_state) == 96);
    assert!(std::mem::offset_of!(RuntimeApi, apply_model_rows) == 104);
    assert!(std::mem::offset_of!(RuntimeApi, cancel_request) == 112);
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn c_header_layout_matches_rust_mirror() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = manifest_dir.join("tests/runtime_abi_parity.c");
    let canonical_include = manifest_dir.join("../../..").join("spec");
    let canonical_header = canonical_include.join("spec/orna_runtime_abi_v1.h");
    let mut command = std::process::Command::new("gcc");
    command.args([
        "-std=c11",
        "-fno-short-enums",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-fsyntax-only",
    ]);
    command.arg("-I").arg(&canonical_include);
    if !canonical_header.is_file() {
        command.arg("-DORNA_RUNTIME_ABI_USE_LOCAL_FIXTURE=1");
    }
    let output = command
        .arg(source)
        .output()
        .expect("gcc is required for the runtime ABI parity proof");

    assert!(
        output.status.success(),
        "runtime ABI C parity failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
