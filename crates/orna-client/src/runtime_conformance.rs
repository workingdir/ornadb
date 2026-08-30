use super::runtime_abi::*;
use orna_core::value::MAX_RUNTIME_VALUE_NODES;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    ffi::{c_char, c_void},
    ptr, slice,
    sync::{
        Arc, Condvar, LazyLock, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, ThreadId},
};
mod headless_fixture;

pub use headless_fixture::{
    HeadlessFixtureError, HeadlessFixtureErrorKind, HeadlessFixtureSession,
};

unsafe impl Sync for StringView {}
unsafe impl Sync for ContractVersion {}
unsafe impl Sync for SinkOffer {}
unsafe impl Sync for Descriptor {}

const ABI_MAJOR: u32 = 1;
const ABI_MINOR: u32 = 0;
const RUNTIME_NAME: &str = "orna-runtime-headless-conformance";
const RUNTIME_VERSION: &str = "1.0.0";
const PLATFORM: &str = "linux-x86_64";
const SINK_NAME: &str = "std.ui.UI";
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
static HANDLE_RESERVATIONS: LazyLock<Mutex<HashSet<Handle>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
const MAX_VIEW_BYTES: usize = 16 * 1024 * 1024;
const MAX_BATCH_OPERATIONS: usize = 1024;

const fn view(bytes: &'static [u8]) -> StringView {
    StringView {
        data: bytes.as_ptr() as *const c_char,
        len: bytes.len(),
    }
}

fn status_message(code: StatusCode) -> &'static [u8] {
    match code {
        StatusCode::Ok => b"ORNA-S000 ok",
        StatusCode::InvalidArgument => b"ORNA-E100 invalid argument",
        StatusCode::Unsupported => b"ORNA-E101 unsupported",
        StatusCode::NotFound => b"ORNA-E102 not found",
        StatusCode::Busy => b"ORNA-E103 busy",
        StatusCode::Cancelled => b"ORNA-E104 cancelled",
        StatusCode::Failed => b"ORNA-E105 failed",
        StatusCode::Internal => b"ORNA-E106 internal",
        StatusCode::StaleRevision => b"ORNA-E107 stale revision",
        _ => b"ORNA-E199 unknown status",
    }
}

fn status(code: StatusCode, _detail: &'static [u8]) -> Status {
    Status {
        code,
        message: view(status_message(code)),
    }
}
fn ok() -> Status {
    status(StatusCode::Ok, b"ok")
}

fn next_unreserved_handle() -> Handle {
    let mut reservations = HANDLE_RESERVATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    loop {
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
        assert_ne!(handle, 0, "handle allocation exhausted");
        if reservations.insert(handle) {
            return handle;
        }
    }
}

fn next_unreserved_alias_handle() -> Handle {
    loop {
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
        assert_ne!(handle, 0, "handle allocation exhausted");
        if !is_reserved_handle(handle) {
            return handle;
        }
    }
}

fn reserve_alias(handle: Handle) -> bool {
    HANDLE_RESERVATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(handle)
}
fn is_reserved_handle(handle: Handle) -> bool {
    HANDLE_RESERVATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&handle)
}

unsafe fn text(view: StringView) -> Option<&'static str> {
    if view.len == 0 {
        return Some("");
    }
    if view.len > MAX_VIEW_BYTES {
        return None;
    }
    if view.data.is_null() {
        return None;
    }
    let bytes = unsafe { slice::from_raw_parts(view.data.cast::<u8>(), view.len) };

    std::str::from_utf8(bytes).ok()
}

unsafe fn owned_text(view: StringView) -> Option<String> {
    unsafe { text(view) }.map(ToOwned::to_owned)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadError {
    AbiMajor(u32),
    AbiMinor(u32),
    NullDescriptor,
    Descriptor(&'static str),
}

fn valid_client_api(client: &ClientApi) -> bool {
    client.log as usize != 0
        && client.emit_runtime_event as usize != 0
        && client.complete_model_request as usize != 0
        && client.fail_model_request as usize != 0
        && client.read_action_metadata as usize != 0
        && client.read_value_debug_json as usize != 0
        && client.monotonic_time_ns as usize != 0
}

fn validate_api(api: &RuntimeApi) -> Result<(), LoadError> {
    if api.abi_major != ABI_MAJOR {
        return Err(LoadError::AbiMajor(api.abi_major));
    }
    if api.abi_minor > ABI_MINOR {
        return Err(LoadError::AbiMinor(api.abi_minor));
    }
    let required_functions = [
        api.describe as usize,
        api.create as usize,
        api.destroy as usize,
        api.start_event_loop as usize,
        api.poll_event_loop as usize,
        api.request_shutdown as usize,
        api.create_surface as usize,
        api.destroy_surface as usize,
        api.apply_ui_batch as usize,
        api.set_surface_visible as usize,
        api.capture_semantic_state as usize,
        api.capture_opaque_state as usize,
        api.apply_model_rows as usize,
        api.cancel_request as usize,
    ];
    if required_functions.into_iter().any(|function| function == 0) {
        return Err(LoadError::Descriptor("runtime API function"));
    }
    let descriptor = unsafe { (api.describe)() };
    if descriptor.is_null() {
        return Err(LoadError::NullDescriptor);
    }
    validate_descriptor(unsafe { &*descriptor })
}

fn validate_descriptor(descriptor: &Descriptor) -> Result<(), LoadError> {
    if descriptor.abi_major != ABI_MAJOR {
        return Err(LoadError::Descriptor("descriptor ABI major"));
    }
    if descriptor.abi_minor > ABI_MINOR {
        return Err(LoadError::Descriptor("descriptor ABI minor"));
    }
    let required_strings = [
        (descriptor.runtime_name, RUNTIME_NAME),
        (descriptor.runtime_version, RUNTIME_VERSION),
        (descriptor.build_id, "test-fixture"),
        (descriptor.platform, PLATFORM),
    ];
    for (value, expected) in required_strings {
        if unsafe { text(value) } != Some(expected) {
            return Err(LoadError::Descriptor("runtime identity"));
        }
    }
    if descriptor.thread_model != ThreadModel::CallerPumps {
        return Err(LoadError::Descriptor("thread model"));
    }
    if descriptor.features & !0x7f != 0 {
        return Err(LoadError::Descriptor("unknown feature"));
    }
    if descriptor.sink_count != 1 || descriptor.sinks.is_null() {
        return Err(LoadError::Descriptor("sink count"));
    }
    let sinks = unsafe { slice::from_raw_parts(descriptor.sinks, descriptor.sink_count) };
    let sink = sinks[0];
    if unsafe { text(sink.type_name) } != Some(SINK_NAME)
        || sink.supports_streaming > 1
        || sink.media_type_count > 16
        || (sink.media_type_count == 0) != sink.media_types.is_null()
    {
        return Err(LoadError::Descriptor("sink offer"));
    }
    if sink.media_type_count != 0 {
        let media_types = unsafe { slice::from_raw_parts(sink.media_types, sink.media_type_count) };
        let mut media_names = HashSet::new();
        for media_type in media_types {
            let Some(media_name) = (unsafe { text(*media_type) }) else {
                return Err(LoadError::Descriptor("sink offer"));
            };
            if media_name.is_empty() || !media_names.insert(media_name.to_owned()) {
                return Err(LoadError::Descriptor("sink offer"));
            }
        }
    }
    if descriptor.contract_count != 1 || descriptor.contracts.is_null() {
        return Err(LoadError::Descriptor("contract count"));
    }
    let contract = unsafe { &*descriptor.contracts };
    let Some(name) = (unsafe { text(contract.name) }) else {
        return Err(LoadError::Descriptor("contract name"));
    };
    if name != SINK_NAME {
        return Err(LoadError::Descriptor("contract name"));
    }
    if contract.major != 1 || contract.minor != 0 {
        return Err(LoadError::Descriptor("contract version"));
    }
    if contract.feature_count > 16 || (contract.feature_count == 0) != contract.features.is_null() {
        return Err(LoadError::Descriptor("contract features"));
    }
    if contract.feature_count != 0 {
        let features = unsafe { slice::from_raw_parts(contract.features, contract.feature_count) };
        let mut feature_names = HashSet::new();
        for feature in features {
            let Some(feature_name) = (unsafe { text(*feature) }) else {
                return Err(LoadError::Descriptor("contract features"));
            };
            if feature_name.is_empty() || !feature_names.insert(feature_name.to_owned()) {
                return Err(LoadError::Descriptor("contract features"));
            }
        }
    }
    Ok(())
}

static CONTRACT: ContractVersion = ContractVersion {
    name: view(b"std.ui.UI"),
    major: 1,
    minor: 0,
    features: ptr::null(),
    feature_count: 0,
};

static SINK: SinkOffer = SinkOffer {
    type_name: view(b"std.ui.UI"),
    media_types: ptr::null(),
    media_type_count: 0,
    supports_streaming: 0,
    preference_rank: 0,
};

static DESCRIPTOR: Descriptor = Descriptor {
    abi_major: ABI_MAJOR,
    abi_minor: ABI_MINOR,
    runtime_name: view(b"orna-runtime-headless-conformance"),
    runtime_version: view(b"1.0.0"),
    build_id: view(b"test-fixture"),
    platform: view(b"linux-x86_64"),
    thread_model: ThreadModel::CallerPumps,
    features: 0,
    sinks: &SINK,
    sink_count: 1,
    contracts: &CONTRACT,
    contract_count: 1,
};

static DESCRIBE_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn fixture_describe() -> *const Descriptor {
    DESCRIBE_CALLS.fetch_add(1, Ordering::SeqCst);
    &DESCRIPTOR
}

#[derive(Default)]
struct ReleaseCounters {
    releases: AtomicUsize,
    invalid: AtomicUsize,
}

struct OwnedAllocation {
    bytes: Vec<u8>,
    counters: Arc<ReleaseCounters>,
}

struct AllocationRecord {
    data: usize,
    len: usize,
    counters: Arc<ReleaseCounters>,
    _allocation: Box<OwnedAllocation>,
}

static NEXT_ALLOCATION_OWNER: AtomicUsize = AtomicUsize::new(1);
static ALLOCATIONS: LazyLock<Mutex<HashMap<usize, AllocationRecord>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static UNKNOWN_RELEASES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn release_owned(owner: *mut c_void, data: *mut u8, len: usize) {
    let key = owner as usize;
    let mut allocations = ALLOCATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(record) = allocations.get(&key) else {
        UNKNOWN_RELEASES.fetch_add(1, Ordering::SeqCst);
        return;
    };

    if record.len != len || record.data != data as usize {
        record.counters.invalid.fetch_add(1, Ordering::SeqCst);
        return;
    }

    let record = allocations
        .remove(&key)
        .expect("allocation record should exist");
    record.counters.releases.fetch_add(1, Ordering::SeqCst);
    drop(record);
}

fn owned_bytes(bytes: Vec<u8>, counters: Arc<ReleaseCounters>) -> OwnedBytes {
    let mut allocation = Box::new(OwnedAllocation { bytes, counters });
    let data = if allocation.bytes.is_empty() {
        ptr::null_mut()
    } else {
        allocation.bytes.as_mut_ptr()
    };
    let len = allocation.bytes.len();
    let owner = NEXT_ALLOCATION_OWNER.fetch_add(1, Ordering::SeqCst);
    assert_ne!(owner, 0, "owned allocation owner exhausted");
    let record = AllocationRecord {
        data: data as usize,
        len,
        counters: Arc::clone(&allocation.counters),
        _allocation: allocation,
    };
    ALLOCATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(owner, record);
    OwnedBytes {
        data,
        len,
        owner: owner as *mut c_void,
        release: release_owned,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EventRecord {
    kind: EventKind,
    surface: SurfaceHandle,
    request: RequestHandle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CallbackKind {
    Event(EventRecord),
    Completion(RequestHandle),
    Failure(RequestHandle, StatusCode),
    Terminal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CallbackRecord {
    sequence: u64,
    terminal: bool,
    kind: CallbackKind,
}

#[derive(Default)]
struct CallbackLog {
    events: Vec<EventRecord>,
    action_payloads: Vec<Vec<u8>>,
    completions: Vec<RequestHandle>,
    failures: Vec<(RequestHandle, StatusCode)>,
    sequence: Vec<CallbackRecord>,
    next_sequence: u64,
    terminal: bool,
    reenter: bool,
    reentry_status: Option<StatusCode>,
}

impl CallbackLog {
    fn record(&mut self, kind: CallbackKind) {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("callback sequence exhausted");
        self.sequence.push(CallbackRecord {
            sequence,
            terminal: self.terminal,
            kind,
        });
    }

    fn mark_terminal(&mut self) {
        self.terminal = true;
        self.record(CallbackKind::Terminal);
    }
}

#[derive(Default)]
struct HandleRegistry {
    known_surfaces: HashSet<SurfaceHandle>,
    live_surfaces: HashSet<SurfaceHandle>,
    known_nodes: HashSet<NodeHandle>,
    live_nodes: HashSet<NodeHandle>,
    known_actions: HashSet<ActionHandle>,
    live_actions: HashSet<ActionHandle>,
    known_models: HashSet<ModelHandle>,
    live_models: HashSet<ModelHandle>,
    known_requests: HashSet<RequestHandle>,
    live_requests: HashSet<RequestHandle>,
    node_surfaces: HashMap<NodeHandle, SurfaceHandle>,
    action_surfaces: HashMap<ActionHandle, SurfaceHandle>,
    action_input_types: HashMap<ActionHandle, String>,
    model_surfaces: HashMap<ModelHandle, SurfaceHandle>,
    request_surfaces: HashMap<RequestHandle, SurfaceHandle>,
    request_models: HashMap<RequestHandle, ModelHandle>,
    terminal_requests: HashSet<RequestHandle>,
}

impl HandleRegistry {
    fn register_surface(&mut self, surface: SurfaceHandle) {
        self.known_surfaces.insert(surface);
        self.live_surfaces.insert(surface);
    }

    fn register_node(&mut self, node: NodeHandle, surface: SurfaceHandle) {
        self.known_nodes.insert(node);
        self.live_nodes.insert(node);
        self.node_surfaces.insert(node, surface);
    }

    fn register_action(
        &mut self,
        action: ActionHandle,
        surface: SurfaceHandle,
        input_type: String,
    ) {
        self.known_actions.insert(action);
        self.live_actions.insert(action);
        self.action_surfaces.insert(action, surface);
        self.action_input_types.insert(action, input_type);
    }

    fn register_model(&mut self, model: ModelHandle, surface: SurfaceHandle) {
        self.known_models.insert(model);
        self.live_models.insert(model);
        self.model_surfaces.insert(model, surface);
    }

    fn register_request(
        &mut self,
        request: RequestHandle,
        model: ModelHandle,
        surface: SurfaceHandle,
    ) {
        self.known_requests.insert(request);
        self.live_requests.insert(request);
        self.request_surfaces.insert(request, surface);
        self.request_models.insert(request, model);
    }

    fn retire_surface(&mut self, surface: SurfaceHandle) {
        self.live_surfaces.remove(&surface);
        let nodes = self
            .node_surfaces
            .iter()
            .filter_map(|(node, owner)| (*owner == surface).then_some(*node))
            .collect::<Vec<_>>();
        for node in nodes {
            self.retire_node(node);
        }
        let actions = self
            .action_surfaces
            .iter()
            .filter_map(|(action, owner)| (*owner == surface).then_some(*action))
            .collect::<Vec<_>>();
        for action in actions {
            self.retire_action(action);
        }
        let models = self
            .model_surfaces
            .iter()
            .filter_map(|(model, owner)| (*owner == surface).then_some(*model))
            .collect::<Vec<_>>();
        for model in models {
            self.retire_model(model);
        }
        let requests = self
            .request_surfaces
            .iter()
            .filter_map(|(request, owner)| (*owner == surface).then_some(*request))
            .collect::<Vec<_>>();
        for request in requests {
            self.retire_request(request);
        }
    }

    fn retire_node(&mut self, node: NodeHandle) {
        self.live_nodes.remove(&node);
        self.node_surfaces.remove(&node);
    }

    fn retire_action(&mut self, action: ActionHandle) {
        self.live_actions.remove(&action);
        self.action_surfaces.remove(&action);
        self.action_input_types.remove(&action);
    }

    fn retire_model(&mut self, model: ModelHandle) {
        self.live_models.remove(&model);
        self.model_surfaces.remove(&model);
    }

    fn retire_request(&mut self, request: RequestHandle) {
        self.live_requests.remove(&request);
        self.request_surfaces.remove(&request);
        self.request_models.remove(&request);
    }

    fn check_live(
        handle: Handle,
        known: &HashSet<Handle>,
        live: &HashSet<Handle>,
        kind: &'static [u8],
    ) -> Result<(), Status> {
        if handle == 0 || !known.contains(&handle) {
            return Err(status(StatusCode::InvalidArgument, kind));
        }
        if !live.contains(&handle) {
            return Err(status(StatusCode::NotFound, kind));
        }
        Ok(())
    }

    fn check_surface(&self, surface: SurfaceHandle) -> Result<(), Status> {
        Self::check_live(
            surface,
            &self.known_surfaces,
            &self.live_surfaces,
            b"surface handle is not live",
        )
    }

    fn check_node(&self, node: NodeHandle) -> Result<(), Status> {
        Self::check_live(
            node,
            &self.known_nodes,
            &self.live_nodes,
            b"node handle is not live",
        )
    }

    fn check_action(&self, action: ActionHandle) -> Result<(), Status> {
        Self::check_live(
            action,
            &self.known_actions,
            &self.live_actions,
            b"action handle is not live",
        )
    }

    fn check_model(&self, model: ModelHandle) -> Result<(), Status> {
        Self::check_live(
            model,
            &self.known_models,
            &self.live_models,
            b"model handle is not live",
        )
    }

    fn check_request(&self, request: RequestHandle) -> Result<(), Status> {
        Self::check_live(
            request,
            &self.known_requests,
            &self.live_requests,
            b"request handle is not live",
        )
    }
    fn claim_request_callback(&mut self, request: RequestHandle) -> Result<(), Status> {
        self.check_request(request)?;
        if !self.terminal_requests.insert(request) {
            return Err(status(
                StatusCode::NotFound,
                b"request callback already completed",
            ));
        }
        Ok(())
    }

    fn check_node_on_surface(
        &self,
        node: NodeHandle,
        surface: SurfaceHandle,
    ) -> Result<(), Status> {
        self.check_node(node)?;
        if self.node_surfaces.get(&node) != Some(&surface) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"node belongs to another surface",
            ));
        }
        Ok(())
    }

    fn check_action_on_surface(
        &self,
        action: ActionHandle,
        surface: SurfaceHandle,
    ) -> Result<(), Status> {
        self.check_action(action)?;
        if self.action_surfaces.get(&action) != Some(&surface) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"action belongs to another surface",
            ));
        }
        Ok(())
    }

    fn check_action_payload_type(
        &self,
        action: ActionHandle,
        payload: ValueRef,
    ) -> Result<(), Status> {
        let Some(actual) = (unsafe { text(payload.type_name) }) else {
            return Err(status(
                StatusCode::InvalidArgument,
                b"invalid action payload",
            ));
        };
        if self.action_input_types.get(&action).map(String::as_str) != Some(actual) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"action payload type mismatch",
            ));
        }
        Ok(())
    }

    fn check_request_model(
        &self,
        request: RequestHandle,
        model: ModelHandle,
    ) -> Result<(), Status> {
        self.check_request(request)?;
        self.check_model(model)?;
        if self.request_models.get(&request) != Some(&model) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"request belongs to another model",
            ));
        }
        Ok(())
    }
}

struct ContextEntry {
    pointer: usize,
    active: bool,
    in_flight: usize,
}

#[derive(Default)]
struct ContextRegistry {
    entries: HashMap<usize, ContextEntry>,
}

static CONTEXT_REGISTRY: LazyLock<(Mutex<ContextRegistry>, Condvar)> =
    LazyLock::new(|| (Mutex::new(ContextRegistry::default()), Condvar::new()));

fn register_context(context: *mut c_void) {
    let mut registry = CONTEXT_REGISTRY
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let key = context as usize;
    assert!(
        registry
            .entries
            .insert(
                key,
                ContextEntry {
                    pointer: key,
                    active: true,
                    in_flight: 0,
                },
            )
            .is_none(),
        "client context is already registered"
    );
}

fn unregister_context(context: *mut c_void) {
    let key = context as usize;
    let mut registry = CONTEXT_REGISTRY
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(entry) = registry.entries.get_mut(&key) {
        entry.active = false;
    } else {
        return;
    }
    loop {
        let in_flight = registry
            .entries
            .get(&key)
            .map_or(0, |entry| entry.in_flight);
        if in_flight == 0 {
            break;
        }
        registry = CONTEXT_REGISTRY
            .1
            .wait(registry)
            .unwrap_or_else(|error| error.into_inner());
    }
    registry.entries.remove(&key);
}

struct ContextCallGuard {
    key: usize,
    pointer: usize,
}

impl ContextCallGuard {
    fn acquire(context: *mut c_void) -> Option<Self> {
        let key = context as usize;
        let mut registry = CONTEXT_REGISTRY
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = registry.entries.get_mut(&key)?;
        if !entry.active {
            return None;
        }
        entry.in_flight += 1;
        Some(Self {
            key,
            pointer: entry.pointer,
        })
    }

    fn context(&self) -> &ClientContext {
        unsafe { &*(self.pointer as *const ClientContext) }
    }
}

impl Drop for ContextCallGuard {
    fn drop(&mut self) {
        let mut registry = CONTEXT_REGISTRY
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = registry.entries.get_mut(&self.key) {
            entry.in_flight -= 1;
            if entry.in_flight == 0 {
                CONTEXT_REGISTRY.1.notify_all();
            }
        }
    }
}

fn with_registered_context<T>(
    context: *mut c_void,
    operation: impl FnOnce(&ClientContext) -> T,
) -> Option<T> {
    let guard = ContextCallGuard::acquire(context)?;
    Some(operation(guard.context()))
}

struct ClientContext {
    log: Mutex<CallbackLog>,
    counters: Arc<ReleaseCounters>,
    runtime: AtomicU64,
    fail_model_callback: AtomicBool,
    handles: Mutex<HandleRegistry>,
}

impl ClientContext {
    fn new() -> Self {
        Self {
            log: Mutex::new(CallbackLog::default()),
            counters: Arc::new(ReleaseCounters::default()),
            runtime: AtomicU64::new(0),
            fail_model_callback: AtomicBool::new(false),
            handles: Mutex::new(HandleRegistry::default()),
        }
    }
}

unsafe extern "C" fn client_log(
    _context: *mut c_void,
    _level: u32,
    _subsystem: StringView,
    _message: StringView,
) {
}

fn validate_callback_event(
    context: &ClientContext,
    event: &RuntimeEvent,
) -> Result<(SurfaceHandle, RequestHandle), Status> {
    let handles = context
        .handles
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match event.kind {
        EventKind::Action => {
            let value = unsafe { event.as_.action };
            handles.check_node_on_surface(value.node, value.surface)?;
            handles.check_action_on_surface(value.action, value.surface)?;
            if !RuntimeState::valid_value_ref(value.payload) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"invalid action payload",
                ));
            }
            handles.check_action_payload_type(value.action, value.payload)?;
            Ok((value.surface, 0))
        }
        EventKind::FocusChanged => {
            let value = unsafe { event.as_.action };
            handles.check_node_on_surface(value.node, value.surface)?;
            if value.action != 0 {
                handles.check_action_on_surface(value.action, value.surface)?;
                if !RuntimeState::valid_value_ref(value.payload) {
                    return Err(status(
                        StatusCode::InvalidArgument,
                        b"invalid focus payload",
                    ));
                }
                handles.check_action_payload_type(value.action, value.payload)?;
            } else if !RuntimeState::valid_value_ref(value.payload) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"invalid focus payload",
                ));
            }
            Ok((value.surface, 0))
        }
        EventKind::LayoutStateChanged => {
            let value = unsafe { event.as_.layout_state };
            handles.check_node_on_surface(value.node, value.surface)?;
            let Some(name) = (unsafe { text(value.semantic_state_name) }) else {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"invalid layout state name",
                ));
            };
            if name.is_empty()
                || !RuntimeState::valid_value_ref(value.semantic_state)
                || !RuntimeState::valid_bytes_view(value.opaque_runtime_state)
            {
                return Err(status(StatusCode::InvalidArgument, b"invalid layout state"));
            }
            Ok((value.surface, 0))
        }
        EventKind::SurfaceClosed => {
            let surface = unsafe { event.as_.surface_closed.surface };
            handles.check_surface(surface)?;
            Ok((surface, 0))
        }
        EventKind::ModelRangeRequest => {
            let value = unsafe { event.as_.range_request };
            handles.check_request_model(value.request, value.model)?;
            if unsafe { text(value.sort_filter_token) }.is_none() {
                return Err(status(StatusCode::InvalidArgument, b"invalid sort filter"));
            }
            Ok((
                *handles
                    .request_surfaces
                    .get(&value.request)
                    .expect("request ownership should exist"),
                value.request,
            ))
        }
        EventKind::ModelChildrenRequest => {
            let value = unsafe { event.as_.children_request };
            handles.check_request_model(value.request, value.model)?;
            if !RuntimeState::valid_value_ref(value.parent_key) {
                return Err(status(StatusCode::InvalidArgument, b"invalid parent key"));
            }
            Ok((
                *handles
                    .request_surfaces
                    .get(&value.request)
                    .expect("request ownership should exist"),
                value.request,
            ))
        }
        EventKind::Diagnostic => {
            let diagnostic = unsafe { event.as_.diagnostic };
            if !RuntimeState::valid_status(diagnostic.status) {
                return Err(status(StatusCode::InvalidArgument, b"invalid diagnostic"));
            }
            Ok((0, 0))
        }
        _ => Err(status(
            StatusCode::InvalidArgument,
            b"unknown runtime event",
        )),
    }
}

unsafe extern "C" fn client_emit_runtime_event(
    context: *mut c_void,
    runtime: RuntimeHandle,
    event: *const RuntimeEvent,
) -> Status {
    if context.is_null() || event.is_null() {
        return status(
            StatusCode::InvalidArgument,
            b"missing event callback argument",
        );
    }
    let Some(result) = with_registered_context(context, |context| {
        if runtime == 0 || context.runtime.load(Ordering::SeqCst) != runtime {
            return status(StatusCode::InvalidArgument, b"foreign runtime handle");
        }
        let event = unsafe { &*event };
        let (surface, request) = match validate_callback_event(context, event) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let action_payload = match event.kind {
            EventKind::Action | EventKind::FocusChanged => {
                let value = unsafe { event.as_.action };
                Some(OwnedValueRef::from_ref(value.payload).canonical_encoding)
            }
            _ => None,
        };
        let reenter = {
            let mut log = context
                .log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if log.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
            let record = EventRecord {
                kind: event.kind,
                surface,
                request,
            };
            if let Some(payload) = action_payload {
                log.action_payloads.push(payload);
            }
            log.events.push(record.clone());
            log.record(CallbackKind::Event(record));
            std::mem::take(&mut log.reenter)
        };
        if reenter {
            let result = unsafe { fixture_poll(runtime, 0) };
            let mut log = context
                .log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            log.reentry_status = Some(result.code);
        }
        ok()
    }) else {
        return status(
            StatusCode::InvalidArgument,
            b"unregistered callback context",
        );
    };
    result
}

unsafe extern "C" fn client_complete_model_request(
    context: *mut c_void,
    request: RequestHandle,
    result: ValueRef,
) -> Status {
    if context.is_null() {
        return status(StatusCode::InvalidArgument, b"missing callback context");
    }
    let Some(result) = with_registered_context(context, |context| {
        {
            let log = context
                .log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if log.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
        }
        {
            let handles = context
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Err(error) = handles.check_request(request) {
                return error;
            }
        }
        if !RuntimeState::valid_value_ref(result) {
            return status(StatusCode::InvalidArgument, b"invalid model result");
        }
        {
            let mut handles = context
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Err(error) = handles.claim_request_callback(request) {
                return error;
            }
        }
        let mut log = context
            .log
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if log.terminal {
            return status(StatusCode::Failed, b"runtime is shut down");
        }
        log.completions.push(request);
        log.record(CallbackKind::Completion(request));
        ok()
    }) else {
        return status(
            StatusCode::InvalidArgument,
            b"unregistered callback context",
        );
    };
    result
}

unsafe extern "C" fn client_fail_model_request(
    context: *mut c_void,
    request: RequestHandle,
    failure: Status,
) -> Status {
    if context.is_null() {
        return status(StatusCode::InvalidArgument, b"missing callback context");
    }
    let Some(result) = with_registered_context(context, |context| {
        {
            let log = context
                .log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if log.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
        }
        {
            let handles = context
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Err(error) = handles.check_request(request) {
                return error;
            }
        }
        if !RuntimeState::valid_status(failure) {
            return status(StatusCode::InvalidArgument, b"invalid model failure");
        }
        if context.fail_model_callback.swap(false, Ordering::SeqCst) {
            return status(StatusCode::Failed, b"callback failure");
        }
        {
            let mut handles = context
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Err(error) = handles.claim_request_callback(request) {
                return error;
            }
        }
        let mut log = context
            .log
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if log.terminal {
            return status(StatusCode::Failed, b"runtime is shut down");
        }
        log.failures.push((request, failure.code));
        log.record(CallbackKind::Failure(request, failure.code));
        ok()
    }) else {
        return status(
            StatusCode::InvalidArgument,
            b"unregistered callback context",
        );
    };
    result
}

unsafe extern "C" fn client_read_action_metadata(
    context: *mut c_void,
    action: ActionHandle,
    output: *mut OwnedBytes,
) -> Status {
    if context.is_null() || output.is_null() {
        return status(StatusCode::InvalidArgument, b"missing metadata argument");
    }
    let Some(result) = with_registered_context(context, |context| {
        {
            let handles = context
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Err(error) = handles.check_action(action) {
                return error;
            }
        }
        unsafe {
            *output = owned_bytes(Vec::new(), Arc::clone(&context.counters));
        }
        ok()
    }) else {
        return status(
            StatusCode::InvalidArgument,
            b"unregistered callback context",
        );
    };
    result
}

unsafe extern "C" fn client_read_value_debug_json(
    context: *mut c_void,
    value: ValueRef,
    output: *mut OwnedBytes,
) -> Status {
    if context.is_null() || output.is_null() {
        return status(StatusCode::InvalidArgument, b"missing value argument");
    }
    let Some(result) = with_registered_context(context, |context| {
        if !RuntimeState::valid_value_ref(value) {
            return status(StatusCode::InvalidArgument, b"invalid value argument");
        }
        unsafe {
            *output = owned_bytes(b"{}".to_vec(), Arc::clone(&context.counters));
        }
        ok()
    }) else {
        return status(
            StatusCode::InvalidArgument,
            b"unregistered callback context",
        );
    };
    result
}

unsafe extern "C" fn client_monotonic_time_ns(_context: *mut c_void) -> u64 {
    0
}

fn client_api(context: *mut ClientContext) -> ClientApi {
    ClientApi {
        abi_major: ABI_MAJOR,
        abi_minor: ABI_MINOR,
        context: context.cast::<c_void>(),
        log: client_log,
        emit_runtime_event: client_emit_runtime_event,
        complete_model_request: client_complete_model_request,
        fail_model_request: client_fail_model_request,
        read_action_metadata: client_read_action_metadata,
        read_value_debug_json: client_read_value_debug_json,
        monotonic_time_ns: client_monotonic_time_ns,
    }
}

#[derive(Clone)]
struct ValueData {
    type_name: String,
    canonical_encoding: Vec<u8>,
}

impl ValueData {
    fn from_ref(value: ValueRef) -> Result<Self, Status> {
        if value.handle != 0 {
            return Err(status(StatusCode::InvalidArgument, b"invalid value handle"));
        }
        let Some(type_name) = (unsafe { text(value.type_name) }) else {
            return Err(status(StatusCode::InvalidArgument, b"invalid value type"));
        };
        let bytes = value.canonical_encoding;
        if type_name.is_empty()
            || bytes.len > MAX_VIEW_BYTES
            || (bytes.len == 0 && !bytes.data.is_null())
            || (bytes.len > 0 && bytes.data.is_null())
        {
            return Err(status(
                StatusCode::InvalidArgument,
                b"invalid value encoding",
            ));
        }
        let canonical_encoding = if bytes.len == 0 {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(bytes.data, bytes.len).to_vec() }
        };
        Ok(Self {
            type_name: type_name.to_owned(),
            canonical_encoding,
        })
    }
}

#[derive(Clone)]
struct ActionBinding {
    action: ActionHandle,
    input_type: String,
}

#[derive(Clone)]
struct NodeState {
    parent: NodeHandle,
    slot: String,
    contract_name: String,
    contract_major: u32,
    contract_minor: u32,
    explicit_key: ValueData,
    properties: BTreeMap<String, ValueData>,
    children: BTreeMap<String, Vec<NodeHandle>>,
    actions: BTreeMap<String, ActionBinding>,
}

fn append_json_string(output: &mut Vec<u8>, value: &str) {
    output.push(b'"');
    for byte in value.bytes() {
        match byte {
            b'"' => output.extend_from_slice(b"\\\""),
            b'\\' => output.extend_from_slice(b"\\\\"),
            0x08 => output.extend_from_slice(b"\\b"),
            b'\n' => output.extend_from_slice(b"\\n"),
            0x0c => output.extend_from_slice(b"\\f"),
            b'\r' => output.extend_from_slice(b"\\r"),
            b'\t' => output.extend_from_slice(b"\\t"),
            0x00..=0x1f => {
                output.extend_from_slice(format!("\\u{byte:04x}").as_bytes());
            }
            _ => output.push(byte),
        }
    }
    output.push(b'"');
}

fn append_json_value(output: &mut Vec<u8>, value: &ValueData) {
    output.extend_from_slice(b"{\"type\":");
    append_json_string(output, &value.type_name);
    output.extend_from_slice(b",\"value\":");
    output.push(b'"');
    for byte in &value.canonical_encoding {
        output.extend_from_slice(format!("{byte:02x}").as_bytes());
    }
    output.extend_from_slice(b"\"}");
}

fn append_node_json(
    output: &mut Vec<u8>,
    node: NodeHandle,
    nodes: &HashMap<NodeHandle, NodeState>,
) {
    let state = nodes.get(&node).expect("node state should exist");
    output.extend_from_slice(b"{\"actions\":{");
    for (index, (event_name, action)) in state.actions.iter().enumerate() {
        if index != 0 {
            output.push(b',');
        }
        append_json_string(output, event_name);
        output.extend_from_slice(b":{\"action_id\":");
        append_json_string(output, &action.action.to_string());
        output.extend_from_slice(b",\"debug_kind\":null,\"input_type\":");
        append_json_string(output, &action.input_type);
        output.push(b'}');
    }
    output.extend_from_slice(b"},\"call_site_id\":null,\"contract\":{\"id\":");
    append_json_string(output, &state.contract_name);
    output.extend_from_slice(b",\"name\":");
    append_json_string(output, &state.contract_name);
    output.extend_from_slice(b",\"version\":");
    append_json_string(
        output,
        &format!("{}.{}", state.contract_major, state.contract_minor),
    );
    output.extend_from_slice(b"},\"function_instance_id\":null,\"key\":");
    append_json_value(output, &state.explicit_key);
    output.extend_from_slice(b",\"kind\":\"node\",\"properties\":{");
    for (index, (property, value)) in state.properties.iter().enumerate() {
        if index != 0 {
            output.push(b',');
        }
        append_json_string(output, property);
        output.push(b':');
        append_json_value(output, value);
    }
    output.extend_from_slice(b"},\"slots\":{");
    for (index, (slot, children)) in state.children.iter().enumerate() {
        if index != 0 {
            output.push(b',');
        }
        append_json_string(output, slot);
        output.extend_from_slice(b":[");
        for (child_index, child) in children.iter().enumerate() {
            if child_index != 0 {
                output.push(b',');
            }
            append_node_json(output, *child, nodes);
        }
        output.push(b']');
    }
    output.extend_from_slice(b"}}");
}

fn encode_surface_state(
    roots: &[NodeHandle],
    nodes: &HashMap<NodeHandle, NodeState>,
) -> Result<Vec<u8>, Status> {
    let mut body = Vec::new();
    match roots {
        [] => body.extend_from_slice(b"{\"kind\":\"empty\"}"),
        [root] => append_node_json(&mut body, *root, nodes),
        roots => {
            body.extend_from_slice(b"{\"children\":[");
            for (index, root) in roots.iter().enumerate() {
                if index != 0 {
                    body.push(b',');
                }
                append_node_json(&mut body, *root, nodes);
            }
            body.extend_from_slice(b"],\"kind\":\"fragment\"}");
        }
    }
    let length = u32::try_from(body.len())
        .map_err(|_| status(StatusCode::InvalidArgument, b"semantic state is too large"))?;
    let mut frame = b"ORNA-UI/1 ".to_vec();
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

fn valid_typed_value(value: &serde_json::Value) -> bool {
    let Some(value) = value.as_object() else {
        return false;
    };
    value.len() == 2
        && value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some()
        && value.contains_key("value")
}

fn valid_contract(value: &serde_json::Value) -> bool {
    let Some(value) = value.as_object() else {
        return false;
    };
    value.len() == 3
        && value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some()
        && value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some()
        && value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .is_some()
}

fn valid_action(value: &serde_json::Value) -> bool {
    let Some(value) = value.as_object() else {
        return false;
    };
    value
        .get("action_id")
        .and_then(serde_json::Value::as_str)
        .is_some()
        && value
            .get("input_type")
            .and_then(serde_json::Value::as_str)
            .is_some()
        && value
            .get("debug_kind")
            .is_none_or(|debug_kind| debug_kind.is_null() || debug_kind.is_string())
}

fn valid_source_origin(value: &serde_json::Value) -> bool {
    let Some(value) = value.as_object() else {
        return value.is_null();
    };
    value
        .keys()
        .all(|key| matches!(key.as_str(), "source_unit_id" | "start" | "end"))
        && value
            .get("source_unit_id")
            .is_none_or(|source_unit_id| source_unit_id.is_string())
        && value
            .get("start")
            .is_none_or(|start| start.as_i64().is_some())
        && value.get("end").is_none_or(|end| end.as_i64().is_some())
}

fn valid_ui_value(value: &serde_json::Value) -> bool {
    // Keep this test-only fixture validator in parity with the public
    // orna-core UI codec: count UI nodes and walk them iteratively so a
    // deeply nested canonical value cannot overflow the call stack.
    let mut pending = vec![value];
    let mut node_count = 0usize;
    while let Some(value) = pending.pop() {
        node_count = match node_count.checked_add(1) {
            Some(node_count) if node_count <= MAX_RUNTIME_VALUE_NODES => node_count,
            _ => return false,
        };

        let Some(value) = value.as_object() else {
            return false;
        };
        match value.get("kind").and_then(serde_json::Value::as_str) {
            Some("empty") if value.len() == 1 => {}
            Some("fragment") => {
                let Some(children) = value.get("children").and_then(serde_json::Value::as_array)
                else {
                    return false;
                };
                if value.len() != 2 {
                    return false;
                }
                pending.extend(children.iter());
            }
            Some("node") => {
                if value.len() < 5 || value.len() > 9 {
                    return false;
                }
                if value.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "kind"
                            | "contract"
                            | "call_site_id"
                            | "function_instance_id"
                            | "key"
                            | "properties"
                            | "slots"
                            | "actions"
                            | "source_origin"
                    )
                }) {
                    return false;
                }
                if !value.get("contract").is_some_and(valid_contract)
                    || !value
                        .get("call_site_id")
                        .is_none_or(|id| id.is_null() || id.is_string())
                    || !value
                        .get("function_instance_id")
                        .is_none_or(|id| id.is_null() || id.is_string())
                {
                    return false;
                }
                let Some(properties) = value
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
                else {
                    return false;
                };
                if !properties.values().all(valid_typed_value) {
                    return false;
                }
                let Some(slots) = value.get("slots").and_then(serde_json::Value::as_object) else {
                    return false;
                };
                for children in slots.values() {
                    let Some(children) = children.as_array() else {
                        return false;
                    };
                    pending.extend(children.iter());
                }
                let Some(actions) = value.get("actions").and_then(serde_json::Value::as_object)
                else {
                    return false;
                };
                if !actions.values().all(valid_action)
                    || !value.get("source_origin").is_none_or(valid_source_origin)
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

fn valid_canonical_frame(frame: &[u8]) -> bool {
    if frame.len() < 14 || &frame[..10] != b"ORNA-UI/1 " {
        return false;
    }
    let Ok(body_length) = u32::try_from(frame.len() - 14) else {
        return false;
    };
    let declared_length = u32::from_be_bytes(
        frame[10..14]
            .try_into()
            .expect("frame length is four bytes"),
    );
    declared_length == body_length
        && serde_json::from_slice::<serde_json::Value>(&frame[14..]).is_ok_and(|value| {
            valid_ui_value(&value)
                && serde_json::to_vec(&value).is_ok_and(|canonical| canonical == frame[14..])
        })
}

struct SurfaceState {
    revision: u64,
    nodes: HashSet<NodeHandle>,
    node_state: HashMap<NodeHandle, NodeState>,
    roots: Vec<NodeHandle>,
    /// Caller-provided operation tokens are only aliases. The fixture owns
    /// the handles that appear in the semantic tree and callback registry.
    node_aliases: HashMap<NodeHandle, NodeHandle>,
    action_aliases: HashMap<ActionHandle, ActionHandle>,
    owned_handles: HashSet<Handle>,
    records: Vec<String>,
    semantic: Vec<u8>,
    visible: bool,
}

impl SurfaceState {
    fn resolve_node(&self, token: NodeHandle) -> Option<NodeHandle> {
        if self.nodes.contains(&token) {
            Some(token)
        } else {
            self.node_aliases
                .get(&token)
                .copied()
                .filter(|node| self.nodes.contains(node))
        }
    }

    fn resolve_action(&self, token: ActionHandle) -> Option<ActionHandle> {
        if self
            .node_state
            .values()
            .any(|node| node.actions.values().any(|binding| binding.action == token))
        {
            Some(token)
        } else {
            self.action_aliases.get(&token).copied().filter(|action| {
                self.node_state.values().any(|node| {
                    node.actions
                        .values()
                        .any(|binding| binding.action == *action)
                })
            })
        }
    }

    fn detach_child(&mut self, child: NodeHandle) {
        self.roots.retain(|node| *node != child);
        for state in self.node_state.values_mut() {
            for children in state.children.values_mut() {
                children.retain(|node| *node != child);
            }
            state.children.retain(|_, children| !children.is_empty());
        }
    }

    fn remove_subtree(
        &mut self,
        root: NodeHandle,
        retired_nodes: &mut Vec<NodeHandle>,
        retired_actions: &mut Vec<ActionHandle>,
    ) -> bool {
        if !self.nodes.contains(&root) {
            return false;
        }
        let mut stack = vec![root];
        let mut removed = Vec::new();
        while let Some(node) = stack.pop() {
            if let Some(state) = self.node_state.get(&node) {
                for children in state.children.values() {
                    stack.extend(children.iter().copied());
                }
            }
            removed.push(node);
        }
        self.detach_child(root);
        for node in removed {
            if let Some(state) = self.node_state.remove(&node) {
                retired_actions.extend(state.actions.values().map(|binding| binding.action));
            }
            self.nodes.remove(&node);
            self.node_aliases.retain(|_, actual| *actual != node);
            retired_nodes.push(node);
        }
        for action in retired_actions.iter().copied() {
            self.action_aliases.retain(|_, actual| *actual != action);
        }
        true
    }
}

#[derive(Clone, Copy)]
struct RequestRecord {
    surface: SurfaceHandle,
    _model: ModelHandle,
}
#[derive(Clone)]
struct OwnedValueRef {
    handle: Handle,
    type_name: Vec<u8>,
    canonical_encoding: Vec<u8>,
}

impl OwnedValueRef {
    fn from_ref(value: ValueRef) -> Self {
        let type_name = unsafe {
            slice::from_raw_parts(value.type_name.data.cast::<u8>(), value.type_name.len)
        }
        .to_vec();
        let canonical_encoding = if value.canonical_encoding.len == 0 {
            Vec::new()
        } else {
            unsafe {
                slice::from_raw_parts(value.canonical_encoding.data, value.canonical_encoding.len)
                    .to_vec()
            }
        };
        Self {
            handle: value.handle,
            type_name,
            canonical_encoding,
        }
    }

    fn as_ref(&self) -> ValueRef {
        ValueRef {
            handle: self.handle,
            type_name: StringView {
                data: self.type_name.as_ptr().cast::<c_char>(),
                len: self.type_name.len(),
            },
            canonical_encoding: BytesView {
                data: if self.canonical_encoding.is_empty() {
                    ptr::null()
                } else {
                    self.canonical_encoding.as_ptr()
                },
                len: self.canonical_encoding.len(),
            },
        }
    }
}

fn owned_string_view(view: StringView) -> Vec<u8> {
    if view.len == 0 {
        Vec::new()
    } else {
        unsafe { slice::from_raw_parts(view.data.cast::<u8>(), view.len).to_vec() }
    }
}

fn owned_bytes_view(view: BytesView) -> Vec<u8> {
    if view.len == 0 {
        Vec::new()
    } else {
        unsafe { slice::from_raw_parts(view.data, view.len).to_vec() }
    }
}

enum OwnedRuntimeEvent {
    Action {
        surface: SurfaceHandle,
        node: NodeHandle,
        action: ActionHandle,
        payload: OwnedValueRef,
    },
    FocusChanged {
        surface: SurfaceHandle,
        node: NodeHandle,
        action: ActionHandle,
        payload: OwnedValueRef,
    },
    LayoutStateChanged {
        surface: SurfaceHandle,
        node: NodeHandle,
        semantic_state_name: Vec<u8>,
        semantic_state: OwnedValueRef,
        opaque_runtime_state: Vec<u8>,
    },
    SurfaceClosed {
        surface: SurfaceHandle,
    },
    ModelRangeRequest {
        request: RequestHandle,
        model: ModelHandle,
        start: u64,
        count: u64,
        sort_filter_token: Vec<u8>,
    },
    ModelChildrenRequest {
        request: RequestHandle,
        model: ModelHandle,
        parent_key: OwnedValueRef,
    },
    Diagnostic {
        code: StatusCode,
        message: Vec<u8>,
    },
}
impl OwnedRuntimeEvent {
    fn from_event(event: &RuntimeEvent) -> Result<Self, Status> {
        match event.kind {
            EventKind::Action => {
                let value = unsafe { event.as_.action };
                Ok(Self::Action {
                    surface: value.surface,
                    node: value.node,
                    action: value.action,
                    payload: OwnedValueRef::from_ref(value.payload),
                })
            }
            EventKind::FocusChanged => {
                let value = unsafe { event.as_.action };
                Ok(Self::FocusChanged {
                    surface: value.surface,
                    node: value.node,
                    action: value.action,
                    payload: OwnedValueRef::from_ref(value.payload),
                })
            }
            EventKind::LayoutStateChanged => {
                let value = unsafe { event.as_.layout_state };
                Ok(Self::LayoutStateChanged {
                    surface: value.surface,
                    node: value.node,
                    semantic_state_name: owned_string_view(value.semantic_state_name),
                    semantic_state: OwnedValueRef::from_ref(value.semantic_state),
                    opaque_runtime_state: owned_bytes_view(value.opaque_runtime_state),
                })
            }
            EventKind::SurfaceClosed => Ok(Self::SurfaceClosed {
                surface: unsafe { event.as_.surface_closed.surface },
            }),
            EventKind::ModelRangeRequest => {
                let value = unsafe { event.as_.range_request };
                Ok(Self::ModelRangeRequest {
                    request: value.request,
                    model: value.model,
                    start: value.start,
                    count: value.count,
                    sort_filter_token: owned_string_view(value.sort_filter_token),
                })
            }
            EventKind::ModelChildrenRequest => {
                let value = unsafe { event.as_.children_request };
                Ok(Self::ModelChildrenRequest {
                    request: value.request,
                    model: value.model,
                    parent_key: OwnedValueRef::from_ref(value.parent_key),
                })
            }
            EventKind::Diagnostic => {
                let status = unsafe { event.as_.diagnostic.status };
                Ok(Self::Diagnostic {
                    code: status.code,
                    message: owned_string_view(status.message),
                })
            }
            _ => Err(status(
                StatusCode::InvalidArgument,
                b"unknown runtime event",
            )),
        }
    }

    fn as_ffi(&self) -> RuntimeEvent {
        match self {
            Self::Action {
                surface,
                node,
                action,
                payload,
            } => RuntimeEvent {
                kind: EventKind::Action,
                as_: RuntimeEventArgs {
                    action: ActionEvent {
                        surface: *surface,
                        node: *node,
                        action: *action,
                        payload: payload.as_ref(),
                    },
                },
            },
            Self::FocusChanged {
                surface,
                node,
                action,
                payload,
            } => RuntimeEvent {
                kind: EventKind::FocusChanged,
                as_: RuntimeEventArgs {
                    action: ActionEvent {
                        surface: *surface,
                        node: *node,
                        action: *action,
                        payload: payload.as_ref(),
                    },
                },
            },
            Self::LayoutStateChanged {
                surface,
                node,
                semantic_state_name,
                semantic_state,
                opaque_runtime_state,
            } => RuntimeEvent {
                kind: EventKind::LayoutStateChanged,
                as_: RuntimeEventArgs {
                    layout_state: LayoutStateEvent {
                        surface: *surface,
                        node: *node,
                        semantic_state_name: StringView {
                            data: semantic_state_name.as_ptr().cast::<c_char>(),
                            len: semantic_state_name.len(),
                        },
                        semantic_state: semantic_state.as_ref(),
                        opaque_runtime_state: BytesView {
                            data: if opaque_runtime_state.is_empty() {
                                ptr::null()
                            } else {
                                opaque_runtime_state.as_ptr()
                            },
                            len: opaque_runtime_state.len(),
                        },
                    },
                },
            },
            Self::SurfaceClosed { surface } => RuntimeEvent {
                kind: EventKind::SurfaceClosed,
                as_: RuntimeEventArgs {
                    surface_closed: SurfaceClosedEvent { surface: *surface },
                },
            },
            Self::ModelRangeRequest {
                request,
                model,
                start,
                count,
                sort_filter_token,
            } => RuntimeEvent {
                kind: EventKind::ModelRangeRequest,
                as_: RuntimeEventArgs {
                    range_request: ModelRangeRequest {
                        request: *request,
                        model: *model,
                        start: *start,
                        count: *count,
                        sort_filter_token: StringView {
                            data: sort_filter_token.as_ptr().cast::<c_char>(),
                            len: sort_filter_token.len(),
                        },
                    },
                },
            },
            Self::ModelChildrenRequest {
                request,
                model,
                parent_key,
            } => RuntimeEvent {
                kind: EventKind::ModelChildrenRequest,
                as_: RuntimeEventArgs {
                    children_request: ModelChildrenRequest {
                        request: *request,
                        model: *model,
                        parent_key: parent_key.as_ref(),
                    },
                },
            },
            Self::Diagnostic { code, message } => RuntimeEvent {
                kind: EventKind::Diagnostic,
                as_: RuntimeEventArgs {
                    diagnostic: DiagnosticEvent {
                        status: Status {
                            code: *code,
                            message: StringView {
                                data: message.as_ptr().cast::<c_char>(),
                                len: message.len(),
                            },
                        },
                    },
                },
            },
        }
    }
}

struct RuntimeState {
    owner: ThreadId,
    handle: RuntimeHandle,
    client: ClientApi,
    shutdown_requested: bool,
    terminal: bool,
    surfaces: HashMap<SurfaceHandle, SurfaceState>,
    requests: HashMap<RequestHandle, RequestRecord>,
    pending_events: VecDeque<OwnedRuntimeEvent>,
    cancelled_requests: HashMap<RequestHandle, RequestRecord>,
    node_tokens: HashMap<NodeHandle, SurfaceHandle>,
    action_tokens: HashMap<ActionHandle, SurfaceHandle>,
    known_handles: HashSet<Handle>,
    retired_handles: HashSet<Handle>,
    known_surfaces: HashSet<SurfaceHandle>,
    known_nodes: HashSet<NodeHandle>,
    known_actions: HashSet<ActionHandle>,
    known_models: HashSet<ModelHandle>,
    known_requests: HashSet<RequestHandle>,
    allocated_nodes: HashSet<NodeHandle>,
    allocated_actions: HashSet<ActionHandle>,
}

unsafe impl Send for RuntimeState {}

impl RuntimeState {
    fn new(client: ClientApi) -> Self {
        let handle = next_unreserved_handle();
        let mut known_handles = HashSet::new();
        known_handles.insert(handle);
        Self {
            owner: thread::current().id(),
            handle,
            client,
            shutdown_requested: false,
            terminal: false,
            surfaces: HashMap::new(),
            requests: HashMap::new(),
            pending_events: VecDeque::new(),
            cancelled_requests: HashMap::new(),
            node_tokens: HashMap::new(),
            action_tokens: HashMap::new(),
            known_handles,
            retired_handles: HashSet::new(),
            known_surfaces: HashSet::new(),
            known_nodes: HashSet::new(),
            known_actions: HashSet::new(),
            known_models: HashSet::new(),
            known_requests: HashSet::new(),
            allocated_nodes: HashSet::new(),
            allocated_actions: HashSet::new(),
        }
    }

    fn context(&self) -> &ClientContext {
        unsafe { &*self.client.context.cast::<ClientContext>() }
    }

    fn next_handle(&mut self) -> Handle {
        let handle = next_unreserved_handle();
        self.known_handles.insert(handle);
        handle
    }

    fn allocate_node_handle(&mut self) -> NodeHandle {
        let handle = self.next_handle();
        self.known_nodes.insert(handle);
        self.allocated_nodes.insert(handle);
        handle
    }

    fn allocate_action_handle(&mut self) -> ActionHandle {
        let handle = self.next_handle();
        self.known_actions.insert(handle);
        self.allocated_actions.insert(handle);
        handle
    }

    fn operational(&self) -> Result<(), Status> {
        if self.terminal || self.shutdown_requested {
            Err(status(StatusCode::Failed, b"runtime is shutting down"))
        } else {
            Ok(())
        }
    }

    fn check_surface(&self, handle: SurfaceHandle) -> Result<(), Status> {
        if handle == 0 || !self.known_surfaces.contains(&handle) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"foreign surface handle",
            ));
        }
        if self.retired_handles.contains(&handle) || !self.surfaces.contains_key(&handle) {
            return Err(status(StatusCode::NotFound, b"surface handle is not live"));
        }
        Ok(())
    }

    fn check_node(&self, handle: NodeHandle) -> Result<(), Status> {
        if handle == 0 || !self.known_nodes.contains(&handle) {
            return Err(status(StatusCode::InvalidArgument, b"foreign node handle"));
        }
        if self.retired_handles.contains(&handle)
            || !self
                .surfaces
                .values()
                .any(|surface| surface.nodes.contains(&handle))
        {
            return Err(status(StatusCode::NotFound, b"node handle is not live"));
        }
        Ok(())
    }

    fn check_action(&self, handle: ActionHandle) -> Result<(), Status> {
        if handle == 0 || !self.known_actions.contains(&handle) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"foreign action handle",
            ));
        }
        if self.retired_handles.contains(&handle)
            || !self.surfaces.values().any(|surface| {
                surface
                    .node_state
                    .values()
                    .any(|node| node.actions.values().any(|action| action.action == handle))
            })
        {
            return Err(status(StatusCode::NotFound, b"action handle is not live"));
        }
        Ok(())
    }

    fn check_model(&self, handle: ModelHandle) -> Result<(), Status> {
        if handle == 0 || !self.known_models.contains(&handle) {
            return Err(status(StatusCode::InvalidArgument, b"foreign model handle"));
        }
        if self.retired_handles.contains(&handle)
            || !self
                .requests
                .values()
                .any(|request| request._model == handle)
        {
            return Err(status(StatusCode::NotFound, b"model handle is not live"));
        }
        Ok(())
    }

    fn check_request(&self, handle: RequestHandle) -> Result<(), Status> {
        if handle == 0 || !self.known_requests.contains(&handle) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"foreign request handle",
            ));
        }
        if self.retired_handles.contains(&handle) || !self.requests.contains_key(&handle) {
            return Err(status(StatusCode::NotFound, b"request handle is not live"));
        }
        Ok(())
    }

    fn check_node_on_surface(
        &self,
        node: NodeHandle,
        surface: SurfaceHandle,
    ) -> Result<(), Status> {
        self.check_surface(surface)?;
        self.check_node(node)?;
        if !self
            .surfaces
            .get(&surface)
            .expect("surface checked above")
            .nodes
            .contains(&node)
        {
            return Err(status(
                StatusCode::InvalidArgument,
                b"node belongs to another surface",
            ));
        }
        Ok(())
    }

    fn check_action_on_surface(
        &self,
        action: ActionHandle,
        surface: SurfaceHandle,
    ) -> Result<(), Status> {
        self.check_surface(surface)?;
        self.check_action(action)?;
        let belongs = self
            .surfaces
            .get(&surface)
            .expect("surface checked above")
            .node_state
            .values()
            .any(|node| {
                node.actions
                    .values()
                    .any(|binding| binding.action == action)
            });
        if !belongs {
            return Err(status(
                StatusCode::InvalidArgument,
                b"action belongs to another surface",
            ));
        }
        Ok(())
    }
    fn check_action_payload_type(
        &self,
        action: ActionHandle,
        surface: SurfaceHandle,
        payload: ValueRef,
    ) -> Result<(), Status> {
        let Some(actual) = (unsafe { text(payload.type_name) }) else {
            return Err(status(
                StatusCode::InvalidArgument,
                b"invalid action payload",
            ));
        };
        let expected = self
            .surfaces
            .get(&surface)
            .expect("surface checked above")
            .node_state
            .values()
            .find_map(|node| {
                node.actions
                    .values()
                    .find(|binding| binding.action == action)
                    .map(|binding| binding.input_type.as_str())
            });
        if expected != Some(actual) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"action payload type mismatch",
            ));
        }
        Ok(())
    }

    fn check_request_model(
        &self,
        request: RequestHandle,
        model: ModelHandle,
    ) -> Result<SurfaceHandle, Status> {
        self.check_request(request)?;
        self.check_model(model)?;
        let record = self.requests.get(&request).expect("request checked above");
        if record._model != model {
            return Err(status(
                StatusCode::InvalidArgument,
                b"request belongs to another model",
            ));
        }
        Ok(record.surface)
    }
    fn resolve_node_token(
        &self,
        surface: SurfaceHandle,
        staged: &SurfaceState,
        token: NodeHandle,
    ) -> Result<NodeHandle, Status> {
        if token == 0 {
            return Err(status(StatusCode::InvalidArgument, b"zero node handle"));
        }
        if let Some(node) = staged.resolve_node(token) {
            return Ok(node);
        }
        if let Some(owner) = self.node_tokens.get(&token) {
            let live_elsewhere = *owner != surface
                && self
                    .surfaces
                    .get(owner)
                    .is_some_and(|state| state.resolve_node(token).is_some());
            return Err(if live_elsewhere {
                status(
                    StatusCode::InvalidArgument,
                    b"node belongs to another surface",
                )
            } else {
                status(StatusCode::NotFound, b"node handle is not live")
            });
        }
        if self.known_nodes.contains(&token) {
            let live_elsewhere = self
                .surfaces
                .iter()
                .any(|(owner, state)| *owner != surface && state.nodes.contains(&token));
            return Err(if live_elsewhere {
                status(
                    StatusCode::InvalidArgument,
                    b"node belongs to another surface",
                )
            } else {
                status(StatusCode::NotFound, b"node handle is not live")
            });
        }
        if self.known_handles.contains(&token) {
            return Err(status(StatusCode::InvalidArgument, b"foreign node handle"));
        }
        if is_reserved_handle(token) {
            return Err(status(StatusCode::InvalidArgument, b"foreign node handle"));
        }
        Err(status(StatusCode::NotFound, b"node handle is not live"))
    }

    fn resolve_action_token(
        &self,
        surface: SurfaceHandle,
        staged: &SurfaceState,
        token: ActionHandle,
    ) -> Result<ActionHandle, Status> {
        if token == 0 {
            return Err(status(StatusCode::InvalidArgument, b"zero action handle"));
        }
        if let Some(action) = staged.resolve_action(token) {
            return Ok(action);
        }
        if let Some(owner) = self.action_tokens.get(&token) {
            let live_elsewhere = *owner != surface
                && self
                    .surfaces
                    .get(owner)
                    .is_some_and(|state| state.resolve_action(token).is_some());
            return Err(if live_elsewhere {
                status(
                    StatusCode::InvalidArgument,
                    b"action belongs to another surface",
                )
            } else {
                status(StatusCode::NotFound, b"action handle is not live")
            });
        }
        if self.known_actions.contains(&token) {
            let live_elsewhere = self
                .surfaces
                .iter()
                .any(|(owner, state)| *owner != surface && state.resolve_action(token).is_some());
            return Err(if live_elsewhere {
                status(
                    StatusCode::InvalidArgument,
                    b"action belongs to another surface",
                )
            } else {
                status(StatusCode::NotFound, b"action handle is not live")
            });
        }
        if self.known_handles.contains(&token) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"foreign action handle",
            ));
        }
        if is_reserved_handle(token) {
            return Err(status(
                StatusCode::InvalidArgument,
                b"foreign action handle",
            ));
        }
        Err(status(StatusCode::NotFound, b"action handle is not live"))
    }

    fn check_runtime(&self, runtime: RuntimeHandle) -> Result<(), Status> {
        if runtime == 0 || self.handle != runtime {
            return Err(status(
                StatusCode::InvalidArgument,
                b"foreign runtime handle",
            ));
        }
        Ok(())
    }

    fn valid_bytes_view(bytes: BytesView) -> bool {
        bytes.len <= MAX_VIEW_BYTES
            && ((bytes.len == 0 && bytes.data.is_null())
                || (bytes.len > 0 && !bytes.data.is_null()))
    }

    fn valid_value_ref(value: ValueRef) -> bool {
        value.handle == 0
            && unsafe { text(value.type_name) }.is_some_and(|name| !name.is_empty())
            && Self::valid_bytes_view(value.canonical_encoding)
    }

    fn valid_status(status: Status) -> bool {
        let valid_code = matches!(
            status.code,
            StatusCode::Ok
                | StatusCode::InvalidArgument
                | StatusCode::Unsupported
                | StatusCode::NotFound
                | StatusCode::Busy
                | StatusCode::Cancelled
                | StatusCode::Failed
                | StatusCode::Internal
                | StatusCode::StaleRevision
        );
        valid_code
            && (unsafe { text(status.message) })
                .is_some_and(|message| message.as_bytes() == status_message(status.code))
    }

    fn validate_event(
        &self,
        event: &RuntimeEvent,
    ) -> Result<(SurfaceHandle, RequestHandle), Status> {
        match event.kind {
            EventKind::Action => {
                let value = unsafe { event.as_.action };
                self.check_node_on_surface(value.node, value.surface)?;
                self.check_action_on_surface(value.action, value.surface)?;
                if !Self::valid_value_ref(value.payload) {
                    return Err(status(
                        StatusCode::InvalidArgument,
                        b"invalid action payload",
                    ));
                }
                self.check_action_payload_type(value.action, value.surface, value.payload)?;
                Ok((value.surface, 0))
            }
            EventKind::FocusChanged => {
                let value = unsafe { event.as_.action };
                self.check_node_on_surface(value.node, value.surface)?;
                if value.action != 0 {
                    self.check_action_on_surface(value.action, value.surface)?;
                    if !Self::valid_value_ref(value.payload) {
                        return Err(status(
                            StatusCode::InvalidArgument,
                            b"invalid focus payload",
                        ));
                    }
                    self.check_action_payload_type(value.action, value.surface, value.payload)?;
                } else if !Self::valid_value_ref(value.payload) {
                    return Err(status(
                        StatusCode::InvalidArgument,
                        b"invalid focus payload",
                    ));
                }
                Ok((value.surface, 0))
            }
            EventKind::LayoutStateChanged => {
                let value = unsafe { event.as_.layout_state };
                self.check_node_on_surface(value.node, value.surface)?;
                let Some(name) = (unsafe { text(value.semantic_state_name) }) else {
                    return Err(status(
                        StatusCode::InvalidArgument,
                        b"invalid layout state name",
                    ));
                };
                if name.is_empty()
                    || !Self::valid_value_ref(value.semantic_state)
                    || !Self::valid_bytes_view(value.opaque_runtime_state)
                {
                    return Err(status(StatusCode::InvalidArgument, b"invalid layout state"));
                }
                Ok((value.surface, 0))
            }
            EventKind::SurfaceClosed => {
                let surface = unsafe { event.as_.surface_closed.surface };
                self.check_surface(surface)?;
                Ok((surface, 0))
            }
            EventKind::ModelRangeRequest => {
                let value = unsafe { event.as_.range_request };
                let surface = self.check_request_model(value.request, value.model)?;
                if unsafe { text(value.sort_filter_token) }.is_none() {
                    return Err(status(StatusCode::InvalidArgument, b"invalid sort filter"));
                }
                Ok((surface, value.request))
            }
            EventKind::ModelChildrenRequest => {
                let value = unsafe { event.as_.children_request };
                let surface = self.check_request_model(value.request, value.model)?;
                if !Self::valid_value_ref(value.parent_key) {
                    return Err(status(StatusCode::InvalidArgument, b"invalid parent key"));
                }
                Ok((surface, value.request))
            }
            EventKind::Diagnostic => {
                let diagnostic = unsafe { event.as_.diagnostic };
                if !Self::valid_status(diagnostic.status) {
                    return Err(status(StatusCode::InvalidArgument, b"invalid diagnostic"));
                }
                Ok((0, 0))
            }
            _ => Err(status(
                StatusCode::InvalidArgument,
                b"unknown runtime event",
            )),
        }
    }

    fn counters(&self) -> Arc<ReleaseCounters> {
        Arc::clone(&self.context().counters)
    }

    fn emit(&mut self, event: RuntimeEvent) -> Status {
        if let Err(error) = self.validate_event(&event) {
            return error;
        }
        let event = match OwnedRuntimeEvent::from_event(&event) {
            Ok(event) => event,
            Err(error) => return error,
        };
        self.pending_events.push_back(event);
        ok()
    }

    fn drain_events(&mut self) -> Status {
        while let Some(event) = self.pending_events.pop_front() {
            let event = event.as_ffi();
            let result = unsafe {
                (self.client.emit_runtime_event)(self.client.context, self.handle, &event)
            };
            if result.code != StatusCode::Ok {
                return result;
            }
        }
        ok()
    }

    fn create_surface(
        &mut self,
        options: *const SurfaceCreateOptions,
        output: *mut SurfaceHandle,
    ) -> Status {
        if let Err(error) = self.operational() {
            return error;
        }
        if options.is_null() || output.is_null() {
            return status(StatusCode::InvalidArgument, b"missing surface argument");
        }
        let options = unsafe { &*options };
        let Some(kind) = (unsafe { text(options.surface_kind) }) else {
            return status(StatusCode::InvalidArgument, b"invalid surface kind");
        };
        if kind.is_empty() {
            return status(StatusCode::InvalidArgument, b"empty surface kind");
        }
        if options.opaque_runtime_restore_state.len > 0 {
            return status(
                StatusCode::Unsupported,
                b"opaque runtime restore state is unsupported",
            );
        }
        if !Self::valid_bytes_view(options.opaque_runtime_restore_state) {
            return status(StatusCode::InvalidArgument, b"invalid restore state");
        }
        let handle = self.next_handle();
        self.known_surfaces.insert(handle);
        self.surfaces.insert(
            handle,
            SurfaceState {
                revision: 0,
                nodes: HashSet::new(),
                node_state: HashMap::new(),
                roots: Vec::new(),
                node_aliases: HashMap::new(),
                action_aliases: HashMap::new(),
                owned_handles: HashSet::new(),
                records: Vec::new(),
                semantic: encode_surface_state(&[], &HashMap::new())
                    .expect("empty semantic state should encode"),
                visible: false,
            },
        );
        self.context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .register_surface(handle);
        unsafe {
            *output = handle;
        }
        ok()
    }

    fn cancel_requests_for_surface(&mut self, surface: SurfaceHandle) -> Status {
        let requests = self
            .requests
            .iter()
            .filter_map(|(request, record)| {
                (record.surface == surface).then_some((*request, *record))
            })
            .collect::<Vec<_>>();
        let result = self.drain_events();
        if result.code != StatusCode::Ok {
            return result;
        }
        for (request, record) in requests {
            let failure = status(StatusCode::Cancelled, b"request cancelled with surface");
            let result =
                unsafe { (self.client.fail_model_request)(self.client.context, request, failure) };
            if result.code != StatusCode::Ok {
                // Keep ownership until the cancellation outcome is delivered so a failed
                // callback can be retried by a later teardown or shutdown attempt.
                return result;
            }
            self.requests.remove(&request);
            self.retired_handles.insert(request);
            self.retired_handles.insert(record._model);
            let mut handles = self
                .context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            handles.retire_request(request);
            handles.retire_model(record._model);
            drop(handles);
        }
        ok()
    }
    fn destroy_surface(&mut self, handle: SurfaceHandle) -> Status {
        if let Err(error) = self.operational() {
            return error;
        }
        if let Err(error) = self.check_surface(handle) {
            return error;
        }
        let owned_handles = self
            .surfaces
            .get(&handle)
            .expect("surface checked above")
            .owned_handles
            .clone();
        let result = self.cancel_requests_for_surface(handle);
        if result.code != StatusCode::Ok {
            return result;
        }
        let event = RuntimeEvent {
            kind: EventKind::SurfaceClosed,
            as_: RuntimeEventArgs {
                surface_closed: SurfaceClosedEvent { surface: handle },
            },
        };
        let result = self.emit(event);
        if result.code != StatusCode::Ok {
            return result;
        }
        let result = self.drain_events();
        if result.code != StatusCode::Ok {
            return result;
        }
        self.surfaces
            .remove(&handle)
            .expect("surface should remain until closed event");
        self.retired_handles.insert(handle);
        self.retired_handles.extend(owned_handles);
        self.context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retire_surface(handle);
        result
    }

    fn apply_batch(&mut self, handle: SurfaceHandle, batch: *const UiBatch) -> Status {
        if let Err(error) = self.operational() {
            return error;
        }
        if let Err(error) = self.check_surface(handle) {
            return error;
        }
        if batch.is_null() {
            return status(StatusCode::InvalidArgument, b"missing UI batch");
        }
        let batch = unsafe { &*batch };
        let Some(current) = self.surfaces.get(&handle) else {
            return status(StatusCode::NotFound, b"surface handle is not live");
        };
        if batch.semantic_revision <= current.revision {
            return status(StatusCode::StaleRevision, b"stale semantic revision");
        }
        let Some(expected_revision) = current.revision.checked_add(1) else {
            return status(StatusCode::InvalidArgument, b"semantic revision exhausted");
        };
        if batch.semantic_revision != expected_revision {
            return status(StatusCode::InvalidArgument, b"semantic revision gap");
        }
        if batch.operation_count == 0
            || batch.operation_count > MAX_BATCH_OPERATIONS
            || batch.operations.is_null()
        {
            return status(StatusCode::InvalidArgument, b"invalid UI batch");
        }
        let operations = unsafe { slice::from_raw_parts(batch.operations, batch.operation_count) };
        let mut next = SurfaceState {
            revision: batch.semantic_revision,
            nodes: current.nodes.clone(),
            node_state: current.node_state.clone(),
            roots: current.roots.clone(),
            node_aliases: current.node_aliases.clone(),
            action_aliases: current.action_aliases.clone(),
            owned_handles: current.owned_handles.clone(),
            records: current.records.clone(),
            semantic: current.semantic.clone(),
            visible: current.visible,
        };
        let mut allocated_nodes = Vec::new();
        let mut allocated_actions = Vec::new();
        let mut allocated_action_inputs = Vec::new();
        let mut retired_nodes = Vec::new();
        let mut retired_actions = Vec::new();
        let mut reserved_node_tokens = Vec::new();
        let mut reserved_action_tokens = Vec::new();
        let result: Result<(), Status> = (|| {
            for operation in operations {
                match operation.kind {
                    UiOperationKind::MountNode => {
                        let value = unsafe { operation.as_.mount_node };
                        let Some(contract) = (unsafe { owned_text(value.contract_name) }) else {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"invalid node contract",
                            ));
                        };
                        let Some(slot) = (unsafe { owned_text(value.slot) }) else {
                            return Err(status(StatusCode::InvalidArgument, b"invalid node slot"));
                        };
                        let parent = if value.parent == 0 {
                            0
                        } else {
                            self.resolve_node_token(handle, &next, value.parent)
                                .map_err(|error| {
                                    if error.code == StatusCode::NotFound
                                        && !self.known_handles.contains(&value.parent)
                                    {
                                        status(StatusCode::InvalidArgument, b"invalid mount parent")
                                    } else {
                                        error
                                    }
                                })?
                        };
                        if value.node == 0
                            || next.node_aliases.contains_key(&value.node)
                            || next.nodes.contains(&value.node)
                            || slot.is_empty()
                            || contract != SINK_NAME
                            || value.contract_major != 1
                            || value.contract_minor != 0
                        {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"invalid mount operation",
                            ));
                        }
                        if let Some(owner) = self.node_tokens.get(&value.node) {
                            return Err(
                                if *owner == handle || !self.surfaces.contains_key(owner) {
                                    status(StatusCode::NotFound, b"node handle is not live")
                                } else {
                                    status(
                                        StatusCode::InvalidArgument,
                                        b"node belongs to another surface",
                                    )
                                },
                            );
                        }
                        if self.retired_handles.contains(&value.node) {
                            return Err(status(StatusCode::NotFound, b"node handle is not live"));
                        }
                        if self.known_handles.contains(&value.node) {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"foreign node handle",
                            ));
                        }
                        if self.action_tokens.contains_key(&value.node) {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"foreign node handle",
                            ));
                        }
                        let ordinal_limit = if parent == 0 {
                            next.roots.len()
                        } else {
                            next.node_state
                                .get(&parent)
                                .and_then(|state| state.children.get(&slot))
                                .map_or(0, Vec::len)
                        };
                        if value.ordinal > ordinal_limit {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"invalid mount ordinal",
                            ));
                        }
                        let explicit_key = ValueData::from_ref(value.explicit_key)?;
                        if !reserve_alias(value.node) {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"foreign node handle",
                            ));
                        }
                        reserved_node_tokens.push(value.node);
                        self.node_tokens.insert(value.node, handle);
                        let actual = self.allocate_node_handle();
                        allocated_nodes.push(actual);
                        next.nodes.insert(actual);
                        next.node_aliases.insert(value.node, actual);
                        next.owned_handles.insert(actual);
                        next.node_state.insert(
                            actual,
                            NodeState {
                                parent,
                                slot: slot.clone(),
                                contract_name: contract,
                                contract_major: value.contract_major,
                                contract_minor: value.contract_minor,
                                explicit_key,
                                properties: BTreeMap::new(),
                                children: BTreeMap::new(),
                                actions: BTreeMap::new(),
                            },
                        );
                        if parent == 0 {
                            next.roots.insert(value.ordinal, actual);
                        } else {
                            next.node_state
                                .get_mut(&parent)
                                .expect("validated mount parent")
                                .children
                                .entry(slot.clone())
                                .or_default()
                                .insert(value.ordinal, actual);
                        }
                        next.records
                            .push(format!("mount:{actual}:{slot}:{}", value.ordinal));
                    }
                    UiOperationKind::UnmountNode => {
                        let token = unsafe { operation.as_.unmount_node };
                        let node = self.resolve_node_token(handle, &next, token)?;
                        if !next.remove_subtree(node, &mut retired_nodes, &mut retired_actions) {
                            return Err(status(StatusCode::NotFound, b"node handle is not live"));
                        }
                        next.records.push(format!("unmount:{node}"));
                    }
                    UiOperationKind::SetProperty | UiOperationKind::ClearProperty => {
                        let value = unsafe { operation.as_.set_property };
                        let Some(property) = (unsafe { owned_text(value.property) }) else {
                            return Err(status(StatusCode::InvalidArgument, b"invalid property"));
                        };
                        let node = self.resolve_node_token(handle, &next, value.node)?;
                        if property.is_empty() {
                            return Err(status(StatusCode::InvalidArgument, b"invalid property"));
                        }
                        let state = next.node_state.get_mut(&node).expect("live node state");
                        if operation.kind == UiOperationKind::SetProperty {
                            state
                                .properties
                                .insert(property.clone(), ValueData::from_ref(value.value)?);
                        } else {
                            state.properties.remove(&property);
                        }
                        next.records.push(format!(
                            "property:{}:{}:{}",
                            operation.kind.0, node, property
                        ));
                    }
                    UiOperationKind::InsertChild
                    | UiOperationKind::RemoveChild
                    | UiOperationKind::MoveChild => {
                        let value = unsafe { operation.as_.child };
                        let Some(slot) = (unsafe { owned_text(value.slot) }) else {
                            return Err(status(StatusCode::InvalidArgument, b"invalid child slot"));
                        };
                        let parent = self.resolve_node_token(handle, &next, value.parent)?;
                        let child = self.resolve_node_token(handle, &next, value.child)?;
                        if slot.is_empty() || parent == child {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"invalid child operation",
                            ));
                        }
                        let mut ancestor = parent;
                        while ancestor != 0 {
                            if ancestor == child {
                                return Err(status(StatusCode::InvalidArgument, b"child cycle"));
                            }
                            ancestor = next
                                .node_state
                                .get(&ancestor)
                                .map_or(0, |state| state.parent);
                        }
                        let kind = operation.kind;
                        if kind == UiOperationKind::RemoveChild {
                            let valid = next
                                .node_state
                                .get(&parent)
                                .and_then(|state| state.children.get(&slot))
                                .and_then(|children| children.get(value.ordinal))
                                == Some(&child);
                            if !valid {
                                return Err(status(
                                    StatusCode::NotFound,
                                    b"child is not mounted in slot",
                                ));
                            }
                            next.detach_child(child);
                            let child_state =
                                next.node_state.get_mut(&child).expect("live child state");
                            child_state.parent = 0;
                            child_state.slot.clear();
                        } else {
                            let attached = next
                                .node_state
                                .get(&child)
                                .is_some_and(|state| state.parent != 0)
                                || next.roots.contains(&child);
                            if kind == UiOperationKind::InsertChild && attached {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"child is already mounted",
                                ));
                            }
                            if kind == UiOperationKind::MoveChild && !attached {
                                return Err(status(StatusCode::NotFound, b"child is not mounted"));
                            }
                            next.detach_child(child);
                            let ordinal_limit = next
                                .node_state
                                .get(&parent)
                                .and_then(|state| state.children.get(&slot))
                                .map_or(0, Vec::len);
                            if value.ordinal > ordinal_limit {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid child ordinal",
                                ));
                            }
                            next.node_state
                                .get_mut(&parent)
                                .expect("validated child parent")
                                .children
                                .entry(slot.clone())
                                .or_default()
                                .insert(value.ordinal, child);
                            let child_state =
                                next.node_state.get_mut(&child).expect("live child state");
                            child_state.parent = parent;
                            child_state.slot = slot.clone();
                        }
                        next.records.push(format!(
                            "child:{}:{}:{}:{}",
                            kind.0, parent, child, value.ordinal
                        ));
                    }
                    UiOperationKind::BindAction | UiOperationKind::UnbindAction => {
                        let value = unsafe { operation.as_.bind_action };
                        let Some(event_name) = (unsafe { owned_text(value.event_name) }) else {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"invalid action event",
                            ));
                        };
                        let Some(input_type) = (unsafe { owned_text(value.input_type) }) else {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"invalid action input type",
                            ));
                        };
                        let node = self.resolve_node_token(handle, &next, value.node)?;
                        if event_name.is_empty() {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"invalid action event",
                            ));
                        }
                        if operation.kind == UiOperationKind::BindAction {
                            if value.action == 0
                                || next.action_aliases.contains_key(&value.action)
                                || next.resolve_action(value.action).is_some()
                                || input_type.is_empty()
                            {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid action operation",
                                ));
                            }
                            if let Some(owner) = self.action_tokens.get(&value.action) {
                                return Err(
                                    if *owner == handle || !self.surfaces.contains_key(owner) {
                                        status(StatusCode::NotFound, b"action handle is not live")
                                    } else {
                                        status(
                                            StatusCode::InvalidArgument,
                                            b"action belongs to another surface",
                                        )
                                    },
                                );
                            }
                            if self.retired_handles.contains(&value.action) {
                                return Err(status(
                                    StatusCode::NotFound,
                                    b"action handle is not live",
                                ));
                            }
                            if self.known_handles.contains(&value.action) {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"foreign action handle",
                                ));
                            }
                            if self.node_tokens.contains_key(&value.action) {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"foreign action handle",
                                ));
                            }
                            if next
                                .node_state
                                .get(&node)
                                .expect("live node state")
                                .actions
                                .contains_key(&event_name)
                            {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"action event is already bound",
                                ));
                            }
                            if !reserve_alias(value.action) {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"foreign action handle",
                                ));
                            }
                            reserved_action_tokens.push(value.action);
                            self.action_tokens.insert(value.action, handle);
                            let actual = self.allocate_action_handle();
                            allocated_actions.push(actual);
                            next.action_aliases.insert(value.action, actual);
                            allocated_action_inputs.push((actual, input_type.clone()));
                            next.owned_handles.insert(actual);
                            next.node_state
                                .get_mut(&node)
                                .expect("live node state")
                                .actions
                                .insert(
                                    event_name.clone(),
                                    ActionBinding {
                                        action: actual,
                                        input_type,
                                    },
                                );
                            next.records
                                .push(format!("action:{node}:{actual}:{event_name}"));
                        } else {
                            let actual = self.resolve_action_token(handle, &next, value.action)?;
                            let binding_matches = next
                                .node_state
                                .get(&node)
                                .and_then(|state| state.actions.get(&event_name))
                                .is_some_and(|binding| binding.action == actual);
                            if !binding_matches {
                                return Err(status(
                                    StatusCode::NotFound,
                                    b"action handle is not bound",
                                ));
                            }
                            next.node_state
                                .get_mut(&node)
                                .expect("live node state")
                                .actions
                                .remove(&event_name);
                            next.action_aliases.retain(|_, mapped| *mapped != actual);
                            retired_actions.push(actual);
                            next.records
                                .push(format!("action:{}:{}:{}", node, actual, event_name));
                        }
                    }
                    UiOperationKind::SetFocus | UiOperationKind::SetAccessibility => {
                        return Err(status(
                            StatusCode::Unsupported,
                            b"operation is not in the fixture",
                        ));
                    }
                    _ => {
                        return Err(status(StatusCode::InvalidArgument, b"unknown UI operation"));
                    }
                }
            }
            next.semantic = encode_surface_state(&next.roots, &next.node_state)?;
            Ok(())
        })();
        if let Err(error) = result {
            for token in &reserved_node_tokens {
                self.node_tokens.remove(token);
            }
            for token in &reserved_action_tokens {
                self.action_tokens.remove(token);
            }
            let mut reservations = HANDLE_RESERVATIONS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for token in reserved_node_tokens
                .iter()
                .chain(reserved_action_tokens.iter())
            {
                reservations.remove(token);
            }
            for node in &allocated_nodes {
                self.known_handles.remove(node);
                self.known_nodes.remove(node);
                self.allocated_nodes.remove(node);
                reservations.remove(node);
            }
            for action in &allocated_actions {
                self.known_handles.remove(action);
                self.known_actions.remove(action);
                self.allocated_actions.remove(action);
                reservations.remove(action);
            }
            return error;
        }
        self.retired_handles.extend(retired_nodes.iter().copied());
        self.retired_handles.extend(retired_actions.iter().copied());
        let surface = self
            .surfaces
            .get_mut(&handle)
            .expect("surface checked above");
        *surface = next;
        let mut handles = self
            .context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for node in allocated_nodes {
            handles.register_node(node, handle);
        }
        for (action, input_type) in allocated_action_inputs {
            handles.register_action(action, handle, input_type);
        }
        for node in retired_nodes {
            handles.retire_node(node);
        }
        for action in retired_actions {
            handles.retire_action(action);
        }
        ok()
    }

    fn capture(&self, handle: SurfaceHandle, output: *mut OwnedBytes) -> Status {
        if self.terminal {
            return status(StatusCode::Failed, b"runtime is shut down");
        }
        if output.is_null() {
            return status(StatusCode::InvalidArgument, b"missing output");
        }
        if let Err(error) = self.check_surface(handle) {
            return error;
        }
        let surface = self.surfaces.get(&handle).expect("surface checked above");
        if !valid_canonical_frame(&surface.semantic) {
            return status(StatusCode::Internal, b"invalid semantic state frame");
        }
        unsafe {
            *output = owned_bytes(surface.semantic.clone(), self.counters());
        }
        ok()
    }

    fn set_visible(&mut self, handle: SurfaceHandle, visible: u8) -> Status {
        if let Err(error) = self.operational() {
            return error;
        }
        if visible > 1 {
            return status(StatusCode::InvalidArgument, b"invalid visibility");
        }
        if let Err(error) = self.check_surface(handle) {
            return error;
        }
        self.surfaces
            .get_mut(&handle)
            .expect("surface checked above")
            .visible = visible != 0;
        ok()
    }

    fn start_model_request(
        &mut self,
        surface: SurfaceHandle,
    ) -> Result<(ModelHandle, RequestHandle), Status> {
        self.operational()?;
        self.check_surface(surface)?;
        let model = self.next_handle();
        let request = self.next_handle();
        self.known_models.insert(model);
        self.known_requests.insert(request);
        self.requests.insert(
            request,
            RequestRecord {
                surface,
                _model: model,
            },
        );
        self.context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .register_model(model, surface);
        self.context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .register_request(request, model, surface);
        let event = RuntimeEvent {
            kind: EventKind::ModelRangeRequest,
            as_: RuntimeEventArgs {
                range_request: ModelRangeRequest {
                    request,
                    model,
                    start: 0,
                    count: 16,
                    sort_filter_token: view(b"fixture"),
                },
            },
        };
        let result = self.emit(event);
        if result.code != StatusCode::Ok {
            self.requests.remove(&request);
            if let Some(state) = self.surfaces.get_mut(&surface) {
                state.owned_handles.remove(&model);
                state.owned_handles.remove(&request);
            }
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_request(request);
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_model(model);
            self.retired_handles.insert(model);
            self.retired_handles.insert(request);
            return Err(result);
        }
        Ok((model, request))
    }

    fn cancel_request(&mut self, request: RequestHandle) -> Status {
        if self.terminal {
            return status(StatusCode::Failed, b"runtime is shut down");
        }
        if self.cancelled_requests.contains_key(&request) {
            return status(StatusCode::Cancelled, b"request is already cancelled");
        }
        if let Err(error) = self.check_request(request) {
            return error;
        }
        let result = self.drain_events();
        if result.code != StatusCode::Ok {
            return result;
        }
        let record = self
            .requests
            .get(&request)
            .copied()
            .expect("request checked above");
        let failure = status(StatusCode::Cancelled, b"request cancelled");
        let result =
            unsafe { (self.client.fail_model_request)(self.client.context, request, failure) };
        if result.code != StatusCode::Ok {
            return result;
        }
        self.requests.remove(&request);
        self.cancelled_requests.insert(request, record);
        if let Some(surface) = self.surfaces.get_mut(&record.surface) {
            surface.owned_handles.remove(&request);
            surface.owned_handles.remove(&record._model);
        }
        self.retired_handles.insert(request);
        self.retired_handles.insert(record._model);
        self.context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retire_request(request);
        self.context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retire_model(record._model);
        result
    }

    fn apply_model_rows(&mut self, request: RequestHandle, rows: ValueRef) -> Status {
        if self.terminal {
            return status(StatusCode::Failed, b"runtime is shut down");
        }
        if self.cancelled_requests.contains_key(&request) {
            return status(StatusCode::Cancelled, b"request was cancelled");
        }
        let Some(record) = self.requests.get(&request).copied() else {
            return if request == 0 || !self.known_handles.contains(&request) {
                status(StatusCode::InvalidArgument, b"foreign request handle")
            } else {
                status(StatusCode::NotFound, b"request handle is not live")
            };
        };
        let result = self.drain_events();
        if result.code != StatusCode::Ok {
            return result;
        }
        if !Self::valid_value_ref(rows) {
            return status(StatusCode::InvalidArgument, b"invalid model result");
        }
        let result =
            unsafe { (self.client.complete_model_request)(self.client.context, request, rows) };
        self.requests.remove(&request);
        if let Some(surface_state) = self.surfaces.get_mut(&record.surface) {
            surface_state.owned_handles.remove(&request);
            surface_state.owned_handles.remove(&record._model);
        }
        self.retired_handles.insert(request);
        self.retired_handles.insert(record._model);
        self.context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retire_request(request);
        self.context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retire_model(record._model);
        result
    }

    fn emit_diagnostic(&mut self, diagnostic: Status) -> Status {
        if self.terminal {
            return status(StatusCode::Failed, b"runtime is shut down");
        }
        let event = RuntimeEvent {
            kind: EventKind::Diagnostic,
            as_: RuntimeEventArgs {
                diagnostic: DiagnosticEvent { status: diagnostic },
            },
        };
        self.emit(event)
    }

    fn shutdown(&mut self) -> Status {
        if self.terminal {
            return ok();
        }
        self.shutdown_requested = true;
        let surfaces = self.surfaces.keys().copied().collect::<Vec<_>>();
        for surface in surfaces {
            let result = self.destroy_surface_for_shutdown(surface);
            if result.code != StatusCode::Ok {
                return result;
            }
        }
        let result = self.drain_events();
        if result.code != StatusCode::Ok {
            return result;
        }
        self.terminal = true;
        let mut log = self
            .context()
            .log
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        log.mark_terminal();
        ok()
    }

    fn destroy_surface_for_shutdown(&mut self, handle: SurfaceHandle) -> Status {
        let Some(owned_handles) = self
            .surfaces
            .get(&handle)
            .map(|surface| surface.owned_handles.clone())
        else {
            return ok();
        };
        let result = self.cancel_requests_for_surface(handle);
        if result.code != StatusCode::Ok {
            return result;
        }
        let event = RuntimeEvent {
            kind: EventKind::SurfaceClosed,
            as_: RuntimeEventArgs {
                surface_closed: SurfaceClosedEvent { surface: handle },
            },
        };
        let result = self.emit(event);
        if result.code != StatusCode::Ok {
            return result;
        }
        let result = self.drain_events();
        if result.code != StatusCode::Ok {
            return result;
        }
        self.surfaces
            .remove(&handle)
            .expect("surface should remain until closed event");
        self.retired_handles.insert(handle);
        self.retired_handles.extend(owned_handles);
        self.context()
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retire_surface(handle);
        result
    }
}

struct GlobalRuntime {
    runtime: Option<RuntimeState>,
}

unsafe impl Send for GlobalRuntime {}

fn global() -> &'static Mutex<GlobalRuntime> {
    static GLOBAL: LazyLock<Mutex<GlobalRuntime>> =
        LazyLock::new(|| Mutex::new(GlobalRuntime { runtime: None }));
    &GLOBAL
}

fn serial_lock() -> MutexGuard<'static, ()> {
    static SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    SERIAL.lock().unwrap_or_else(|error| error.into_inner())
}

fn with_runtime<F>(runtime: RuntimeHandle, operation: F) -> Status
where
    F: FnOnce(&mut RuntimeState) -> Status,
{
    let guard = match global().try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => {
            return status(StatusCode::Busy, b"runtime call is already active");
        }
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
    };
    let mut guard = guard;
    let Some(state) = guard.runtime.as_mut() else {
        return status(StatusCode::InvalidArgument, b"runtime is not loaded");
    };
    if state.owner != thread::current().id() {
        return status(StatusCode::Busy, b"runtime belongs to another thread");
    }
    if let Err(error) = state.check_runtime(runtime) {
        return error;
    }
    operation(state)
}

unsafe extern "C" fn fixture_create(
    options: *const RuntimeCreateOptions,
    output: *mut RuntimeHandle,
) -> Status {
    if options.is_null() || output.is_null() {
        return status(StatusCode::InvalidArgument, b"missing runtime argument");
    }
    if validate_api(&FIXTURE_API).is_err() {
        return status(StatusCode::Internal, b"fixture descriptor is invalid");
    }
    let mut guard = match global().try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => {
            return status(StatusCode::Busy, b"runtime call is already active");
        }
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
    };
    if guard.runtime.is_some() {
        return status(StatusCode::Busy, b"runtime already exists");
    }
    let options = unsafe { &*options };

    if options.client.is_null() {
        return status(StatusCode::InvalidArgument, b"missing client API");
    }
    let client = unsafe { *options.client };
    if client.abi_major != ABI_MAJOR
        || client.abi_minor > ABI_MINOR
        || client.context.is_null()
        || !valid_client_api(&client)
        || with_registered_context(client.context, |_| ()).is_none()
    {
        return status(StatusCode::InvalidArgument, b"incompatible client API");
    }
    let state = RuntimeState::new(client);
    let Some(()) = with_registered_context(client.context, |context| {
        context.runtime.store(state.handle, Ordering::SeqCst);
    }) else {
        return status(StatusCode::InvalidArgument, b"unregistered client context");
    };
    unsafe {
        *output = state.handle;
    }
    guard.runtime = Some(state);
    ok()
}

unsafe extern "C" fn fixture_destroy(runtime: RuntimeHandle) {
    let guard = match global().try_lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    let mut guard = guard;
    let Some(state) = guard.runtime.as_ref() else {
        return;
    };
    if state.owner != thread::current().id() || state.handle != runtime {
        return;
    }
    if !state.terminal || !state.surfaces.is_empty() || !state.requests.is_empty() {
        return;
    }
    let context = state.client.context;
    let Some(()) = with_registered_context(context, |context| {
        context.runtime.store(0, Ordering::SeqCst);
    }) else {
        return;
    };
    guard.runtime = None;
}

unsafe extern "C" fn fixture_start(runtime: RuntimeHandle) -> Status {
    with_runtime(runtime, |state| {
        state.operational().map_or_else(|error| error, |_| ok())
    })
}

unsafe extern "C" fn fixture_poll(runtime: RuntimeHandle, _timeout_ms: u32) -> Status {
    with_runtime(runtime, |state| {
        if let Err(error) = state.operational() {
            return error;
        }
        state.drain_events()
    })
}

unsafe extern "C" fn fixture_request_shutdown(runtime: RuntimeHandle) -> Status {
    with_runtime(runtime, RuntimeState::shutdown)
}

unsafe extern "C" fn fixture_create_surface(
    runtime: RuntimeHandle,
    options: *const SurfaceCreateOptions,
    output: *mut SurfaceHandle,
) -> Status {
    with_runtime(runtime, |state| state.create_surface(options, output))
}

unsafe extern "C" fn fixture_destroy_surface(
    runtime: RuntimeHandle,
    surface: SurfaceHandle,
) -> Status {
    with_runtime(runtime, |state| state.destroy_surface(surface))
}

unsafe extern "C" fn fixture_apply_ui_batch(
    runtime: RuntimeHandle,
    surface: SurfaceHandle,
    batch: *const UiBatch,
) -> Status {
    with_runtime(runtime, |state| state.apply_batch(surface, batch))
}

unsafe extern "C" fn fixture_set_surface_visible(
    runtime: RuntimeHandle,
    surface: SurfaceHandle,
    visible: u8,
) -> Status {
    with_runtime(runtime, |state| state.set_visible(surface, visible))
}

unsafe extern "C" fn fixture_capture_semantic_state(
    runtime: RuntimeHandle,
    surface: SurfaceHandle,
    output: *mut OwnedBytes,
) -> Status {
    with_runtime(runtime, |state| state.capture(surface, output))
}

unsafe extern "C" fn fixture_capture_opaque_state(
    runtime: RuntimeHandle,
    surface: SurfaceHandle,
    output: *mut OwnedBytes,
) -> Status {
    with_runtime(runtime, |state| {
        if state.terminal {
            return status(StatusCode::Failed, b"runtime is shut down");
        }
        if output.is_null() {
            return status(StatusCode::InvalidArgument, b"missing output");
        }
        if let Err(error) = state.check_surface(surface) {
            return error;
        }
        unsafe {
            *output = owned_bytes(Vec::new(), state.counters());
        }
        ok()
    })
}

unsafe extern "C" fn fixture_apply_model_rows(
    runtime: RuntimeHandle,
    request: RequestHandle,
    rows: ValueRef,
) -> Status {
    with_runtime(runtime, |state| state.apply_model_rows(request, rows))
}

unsafe extern "C" fn fixture_cancel_request(
    runtime: RuntimeHandle,
    request: RequestHandle,
) -> Status {
    with_runtime(runtime, |state| state.cancel_request(request))
}

static FIXTURE_API: RuntimeApi = RuntimeApi {
    abi_major: ABI_MAJOR,
    abi_minor: ABI_MINOR,
    describe: fixture_describe,
    create: fixture_create,
    destroy: fixture_destroy,
    start_event_loop: fixture_start,
    poll_event_loop: fixture_poll,
    request_shutdown: fixture_request_shutdown,
    create_surface: fixture_create_surface,
    destroy_surface: fixture_destroy_surface,
    apply_ui_batch: fixture_apply_ui_batch,
    set_surface_visible: fixture_set_surface_visible,
    capture_semantic_state: fixture_capture_semantic_state,
    capture_opaque_state: fixture_capture_opaque_state,
    apply_model_rows: fixture_apply_model_rows,
    cancel_request: fixture_cancel_request,
};

struct FixtureSession {
    _serial: MutexGuard<'static, ()>,
    client: Box<ClientContext>,
    runtime: RuntimeHandle,
}

impl FixtureSession {
    fn new() -> Self {
        Self::new_with_serial(serial_lock())
    }

    fn new_with_serial(serial: MutexGuard<'static, ()>) -> Self {
        {
            let mut guard = global().lock().unwrap_or_else(|error| error.into_inner());
            guard.runtime = None;
        }
        let mut client = Box::new(ClientContext::new());
        let context = (&mut *client) as *mut ClientContext as *mut c_void;
        register_context(context);
        let client_api = client_api(context.cast::<ClientContext>());
        let options = RuntimeCreateOptions {
            client: &client_api,
            locale: view(b"en-GB"),
            timezone: view(b"UTC"),
            theme: view(b"default"),
            accessibility_preferences_json: view(b"{}"),
            runtime_configuration_json: view(b"{}"),
        };
        let mut runtime = 0;
        let result = unsafe { (FIXTURE_API.create)(&options, &mut runtime) };
        if result.code != StatusCode::Ok {
            unregister_context(context);
            panic!("fixture runtime creation failed: {:?}", result.code);
        }
        Self {
            _serial: serial,
            client,
            runtime,
        }
    }

    fn create_surface_result(&self, title: &'static [u8]) -> Result<SurfaceHandle, StatusCode> {
        let options = SurfaceCreateOptions {
            surface_kind: view(b"window"),
            title: view(title),
            state_profile: view(b"local"),
            opaque_runtime_restore_state: BytesView {
                data: ptr::null(),
                len: 0,
            },
        };
        let mut surface = 0;
        let result = unsafe { (FIXTURE_API.create_surface)(self.runtime, &options, &mut surface) };
        if result.code == StatusCode::Ok {
            Ok(surface)
        } else {
            Err(result.code)
        }
    }

    fn create_surface(&self, title: &'static [u8]) -> SurfaceHandle {
        let options = SurfaceCreateOptions {
            surface_kind: view(b"window"),
            title: view(title),
            state_profile: view(b"local"),
            opaque_runtime_restore_state: BytesView {
                data: ptr::null(),
                len: 0,
            },
        };
        let mut surface = 0;
        let result = unsafe { (FIXTURE_API.create_surface)(self.runtime, &options, &mut surface) };
        assert_eq!(result.code, StatusCode::Ok);
        surface
    }

    fn apply(&self, surface: SurfaceHandle, batch: &UiBatch) -> StatusCode {
        unsafe { (FIXTURE_API.apply_ui_batch)(self.runtime, surface, batch) }.code
    }

    fn capture(&self, surface: SurfaceHandle) -> Vec<u8> {
        let mut output = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        let result =
            unsafe { (FIXTURE_API.capture_semantic_state)(self.runtime, surface, &mut output) };
        assert_eq!(result.code, StatusCode::Ok);
        let bytes = if output.len == 0 {
            assert!(
                output.data.is_null(),
                "empty owned bytes must have a null data pointer"
            );
            Vec::new()
        } else {
            assert!(
                !output.data.is_null(),
                "non-empty owned bytes must have a data pointer"
            );
            unsafe { slice::from_raw_parts(output.data, output.len).to_vec() }
        };
        unsafe {
            (output.release)(output.owner, output.data, output.len);
        }
        bytes
    }

    fn capture_result(&self, surface: SurfaceHandle) -> Result<Vec<u8>, StatusCode> {
        let mut output = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        let result =
            unsafe { (FIXTURE_API.capture_semantic_state)(self.runtime, surface, &mut output) };
        if result.code != StatusCode::Ok {
            return Err(result.code);
        }
        let bytes = if output.len == 0 {
            if !output.data.is_null() {
                return Err(StatusCode::Internal);
            }
            Vec::new()
        } else {
            if output.data.is_null() {
                return Err(StatusCode::Internal);
            }
            unsafe { slice::from_raw_parts(output.data, output.len).to_vec() }
        };
        unsafe {
            (output.release)(output.owner, output.data, output.len);
        }
        Ok(bytes)
    }

    fn destroy_surface(&self, surface: SurfaceHandle) -> StatusCode {
        unsafe { (FIXTURE_API.destroy_surface)(self.runtime, surface) }.code
    }

    fn shutdown(&self) -> StatusCode {
        unsafe { (FIXTURE_API.request_shutdown)(self.runtime) }.code
    }

    fn start_model_request(&self, surface: SurfaceHandle) -> (ModelHandle, RequestHandle) {
        self.start_model_request_result(surface)
            .expect("fixture callback should accept model request")
    }

    fn start_model_request_result(
        &self,
        surface: SurfaceHandle,
    ) -> Result<(ModelHandle, RequestHandle), StatusCode> {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let mut guard = guard;
        let state = guard
            .runtime
            .as_mut()
            .expect("fixture runtime should exist");
        state
            .start_model_request(surface)
            .map_err(|error| error.code)
    }
    fn queue_event(&self, event: RuntimeEvent) -> StatusCode {
        let mut guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .runtime
            .as_mut()
            .expect("fixture runtime should exist");
        state.emit(event).code
    }

    fn fail_next_model_callback(&self) {
        self.client
            .fail_model_callback
            .store(true, Ordering::SeqCst);
    }

    fn node_and_action(&self, surface: SurfaceHandle) -> (NodeHandle, ActionHandle) {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let state = guard
            .runtime
            .as_ref()
            .expect("fixture runtime should exist");
        let surface_state = state
            .surfaces
            .get(&surface)
            .expect("surface should be live");
        let node = *surface_state
            .nodes
            .iter()
            .next()
            .expect("surface should have a node");
        let action = surface_state
            .node_state
            .get(&node)
            .and_then(|node| node.actions.values().next())
            .map(|binding| binding.action)
            .expect("node should have an action");
        (node, action)
    }

    fn apply_model_rows(&self, request: RequestHandle) -> StatusCode {
        let rows = ValueRef {
            handle: 0,
            type_name: view(b"std.json.Value"),
            canonical_encoding: BytesView {
                data: ptr::null(),
                len: 0,
            },
        };
        unsafe { (FIXTURE_API.apply_model_rows)(self.runtime, request, rows) }.code
    }

    fn cancel_request(&self, request: RequestHandle) -> StatusCode {
        unsafe { (FIXTURE_API.cancel_request)(self.runtime, request) }.code
    }

    fn emit_diagnostic(&self) -> StatusCode {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        let mut guard = guard;
        let state = guard
            .runtime
            .as_mut()
            .expect("fixture runtime should exist");
        let result = state.emit_diagnostic(status(StatusCode::Failed, b"fixture diagnostic"));
        if result.code != StatusCode::Ok {
            return result.code;
        }
        state.drain_events().code
    }
    fn poll(&self) -> StatusCode {
        unsafe { (FIXTURE_API.poll_event_loop)(self.runtime, 0) }.code
    }

    fn set_reentry(&self) {
        let mut log = self
            .client
            .log
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        log.reenter = true;
    }

    fn callback_log(&self) -> CallbackLogSnapshot {
        let log = self
            .client
            .log
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        CallbackLogSnapshot {
            events: log.events.clone(),
            action_payloads: log.action_payloads.clone(),
            completions: log.completions.clone(),
            failures: log.failures.clone(),
            sequence: log.sequence.clone(),
            terminal: log.terminal,
            reentry_status: log.reentry_status,
        }
    }

    fn release_counts(&self) -> (usize, usize) {
        (
            self.client.counters.releases.load(Ordering::SeqCst),
            self.client.counters.invalid.load(Ordering::SeqCst),
        )
    }
}

impl Drop for FixtureSession {
    fn drop(&mut self) {
        unsafe {
            let _ = (FIXTURE_API.request_shutdown)(self.runtime);
            (FIXTURE_API.destroy)(self.runtime);
        }
        let context = (&mut *self.client) as *mut ClientContext as *mut c_void;
        unregister_context(context);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CallbackLogSnapshot {
    events: Vec<EventRecord>,
    action_payloads: Vec<Vec<u8>>,
    completions: Vec<RequestHandle>,
    failures: Vec<(RequestHandle, StatusCode)>,
    sequence: Vec<CallbackRecord>,
    terminal: bool,
    reentry_status: Option<StatusCode>,
}

fn empty_value() -> ValueRef {
    ValueRef {
        handle: 0,
        type_name: view(b"std.json.Value"),
        canonical_encoding: BytesView {
            data: ptr::null(),
            len: 0,
        },
    }
}
fn mount(node: NodeHandle, parent: NodeHandle, slot: StringView) -> UiOperation {
    UiOperation {
        kind: UiOperationKind::MountNode,
        as_: UiOperationArgs {
            mount_node: MountNode {
                node,
                parent,
                slot,
                ordinal: 0,
                contract_name: view(b"std.ui.UI"),
                contract_major: 1,
                contract_minor: 0,
                explicit_key: empty_value(),
            },
        },
    }
}
fn set_property(node: NodeHandle, property: StringView) -> UiOperation {
    UiOperation {
        kind: UiOperationKind::SetProperty,
        as_: UiOperationArgs {
            set_property: SetProperty {
                node,
                property,
                value: empty_value(),
            },
        },
    }
}
fn batch(revision: u64, operations: &[UiOperation]) -> UiBatch {
    UiBatch {
        semantic_revision: revision,
        operations: operations.as_ptr(),
        operation_count: operations.len(),
    }
}

#[cfg(test)]
mod tests;
