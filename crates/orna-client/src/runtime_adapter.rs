//! Host-owned adapter for the accepted `std.ui.window@1` CLIENT contract.
//!
//! The adapter deliberately implements one closed projection of `ORNA-UI/1`.
//! It does not load a runtime, resolve a database path, or carry any authority;
//! the host supplies an already-created [`RuntimeSession`].

use orna_core::{InvocationId, value::RuntimeValue};
use orna_standard::{
    STD_UI_TYPE_ID, STD_UI_WINDOW_CONTENT_PARAMETER_ID, STD_UI_WINDOW_RUNTIME_CONTRACT,
    STD_UI_WINDOW_TITLE_PARAMETER_ID, UI_MAGIC,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::{
    ClientExternalContractRequest, ClientResourceCompletion, ClientResourceExecutor,
    ClientResourceRequest,
    runtime_loader::{
        AbiActionHandle, AbiNodeHandle, AbiSurfaceHandle, CLIENT_MAX_RUNTIME_BATCH_OPERATIONS,
        CLIENT_MAX_RUNTIME_TEXT_BYTES, CLIENT_MAX_RUNTIME_VALUE_BYTES, RuntimeEventSnapshot,
        RuntimeSession, RuntimeSessionError, RuntimeUiBatch, RuntimeUiOperation, RuntimeValueInput,
    },
};

/// Stable, redacted failure returned by this adapter.
///
/// The underlying runtime status, malformed JSON, argument values, and UI
/// payload bytes intentionally never cross the CLIENT external-contract seam.
const ADAPTER_FAILURE: &str = "runtime_adapter.failed";

/// The provider's closed structural contract set.
const WINDOW_CONTRACT: &str = "std.ui.window";
const TEXT_CONTRACT: &str = "std.ui.text";
const BUTTON_CONTRACT: &str = "std.ui.button";
const PANEL_CONTRACT: &str = "std.ui.panel";
const ROW_CONTRACT: &str = "std.ui.row";
const COLUMN_CONTRACT: &str = "std.ui.column";
const TEXT_INPUT_CONTRACT: &str = "std.ui.text_input";
const TABS_CONTRACT: &str = "std.ui.tabs";

const TEXT_TYPE: &str = "std.text";
const BOOLEAN_TYPE: &str = "std.boolean";
const STANDARD_TEXT_TYPE: &str = "std.types.text";
const STANDARD_BOOLEAN_TYPE: &str = "std.types.boolean";
const JSON_VALUE_TYPE: &str = "std.json.Value";
const CONTENT_SLOT: &str = "content";
const ROOT_SLOT: &str = "root";
const UI_SEMANTIC_REVISION: u64 = 1;
const MAX_UI_NODES: usize = 4096;

/// The action identity associated with one runtime callback handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeActionBinding {
    surface: AbiSurfaceHandle,
    action_id: String,
}

impl RuntimeActionBinding {
    /// Returns the surface that owns the action.
    pub const fn surface(&self) -> AbiSurfaceHandle {
        self.surface
    }

    /// Returns the declared CLIENT action identity.
    pub fn action_id(&self) -> &str {
        &self.action_id
    }
}

/// A host-owned executor for the first production Qt runtime contract.
///
/// `RuntimeSession` is intentionally supplied by the host. In particular,
/// this type has no constructor that accepts a filesystem path, database plan,
/// principal, grant, or environment lookup. Non-UI client work is delegated
/// to the optional fallback executor supplied by the host.
pub struct QtRuntimeExecutor {
    session: RuntimeSession,
    fallback: Option<Box<dyn ClientResourceExecutor>>,
    next_alias: u64,
    active_surfaces: HashSet<AbiSurfaceHandle>,
    pending_events: Vec<RuntimeEventSnapshot>,
    action_bindings: HashMap<AbiActionHandle, RuntimeActionBinding>,
}

impl QtRuntimeExecutor {
    /// Wraps an explicitly-created caller-pumps runtime session.
    pub fn new(session: RuntimeSession) -> Self {
        Self {
            session,
            fallback: None,
            next_alias: 1,
            active_surfaces: HashSet::new(),
            pending_events: Vec::new(),
            action_bindings: HashMap::new(),
        }
    }

    /// Wraps a runtime session and delegates non-UI CLIENT work to `fallback`.
    pub fn with_fallback<F>(session: RuntimeSession, fallback: F) -> Self
    where
        F: ClientResourceExecutor + 'static,
    {
        Self {
            session,
            fallback: Some(Box::new(fallback)),
            next_alias: 1,
            active_surfaces: HashSet::new(),
            pending_events: Vec::new(),
            action_bindings: HashMap::new(),
        }
    }

    /// Processes Qt events on the host-owned runtime thread.
    pub fn poll_runtime(&mut self, timeout_ms: u32) -> Result<(), RuntimeSessionError> {
        self.session.poll_event_loop(timeout_ms)
    }

    /// Drains owned callback snapshots without invoking server actions.
    pub fn drain_runtime_events(&mut self) -> Vec<RuntimeEventSnapshot> {
        let mut events = std::mem::take(&mut self.pending_events);
        events.extend(self.session.drain_events());
        self.note_surface_events(&events);
        events
    }

    fn note_surface_events(&mut self, events: &[RuntimeEventSnapshot]) {
        reconcile_surface_events(&mut self.active_surfaces, &mut self.action_bindings, events);
    }

    /// Resolves a runtime callback handle to its declared CLIENT action.
    pub fn action_binding(&self, action: AbiActionHandle) -> Option<&RuntimeActionBinding> {
        self.action_bindings.get(&action)
    }

    /// Destroys one surface and retires its callback bindings.
    pub fn destroy_surface(
        &mut self,
        surface: AbiSurfaceHandle,
    ) -> Result<(), RuntimeSessionError> {
        let result = self.session.destroy_surface(surface);
        self.active_surfaces.remove(&surface);
        self.action_bindings
            .retain(|_, binding| binding.surface != surface);
        result
    }

    /// Captures the provider's canonical semantic state for one surface.
    pub fn capture_semantic_state(
        &mut self,
        surface: AbiSurfaceHandle,
    ) -> Result<Vec<u8>, RuntimeSessionError> {
        self.session.capture_semantic_state(surface)
    }

    /// Sets the visibility of one adapter-created surface.
    pub fn set_surface_visible(
        &mut self,
        surface: AbiSurfaceHandle,
        visible: bool,
    ) -> Result<(), RuntimeSessionError> {
        self.session.set_surface_visible(surface, visible)
    }

    /// Pumps the caller-owned runtime until every adapter-created surface closes.
    ///
    /// Events remain available through [`Self::drain_runtime_events`] after the
    /// loop returns. This method is intended for a foreground GUI entry point;
    /// interactive hosts can call [`Self::poll_runtime`] directly instead.
    pub fn wait_for_surfaces(&mut self) -> Result<(), RuntimeSessionError> {
        while !self.active_surfaces.is_empty() {
            self.session.poll_event_loop(50)?;
            let events = self.session.drain_events();
            self.note_surface_events(&events);
            self.pending_events.extend(events);
        }
        Ok(())
    }

    /// Requests terminal runtime shutdown through the supplied session.
    pub fn shutdown(&mut self) -> Result<(), RuntimeSessionError> {
        self.session.shutdown()?;
        let events = self.session.drain_events();
        self.note_surface_events(&events);
        self.pending_events.extend(events);
        Ok(())
    }
}

impl ClientResourceExecutor for QtRuntimeExecutor {
    fn bind_current_invocation(&mut self, invocation: InvocationId) {
        if let Some(fallback) = self.fallback.as_mut() {
            fallback.bind_current_invocation(invocation);
        }
    }
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        match self.fallback.as_mut() {
            Some(fallback) => fallback.execute(request),
            None => request.failed(ADAPTER_FAILURE.to_owned()),
        }
    }

    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        self.fallback.as_mut().and_then(|fallback| fallback.poll())
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        match self.fallback.as_mut() {
            Some(fallback) => fallback.cancel(request),
            None => request.pending(),
        }
    }

    fn abandon(&mut self, request: ClientResourceRequest) -> Result<(), String> {
        match self.fallback.as_mut() {
            Some(fallback) => fallback.abandon(request),
            None => Err(ADAPTER_FAILURE.to_owned()),
        }
    }

    fn cancel_pending(&mut self) -> Option<ClientResourceCompletion> {
        self.fallback
            .as_mut()
            .and_then(|fallback| fallback.cancel_pending())
    }

    fn inspect(&mut self, request: crate::ClientInspectRequest) -> Result<RuntimeValue, String> {
        match self.fallback.as_mut() {
            Some(fallback) => fallback.inspect(request),
            None => Err(ADAPTER_FAILURE.to_owned()),
        }
    }

    fn external_contract(
        &mut self,
        request: ClientExternalContractRequest,
    ) -> Result<RuntimeValue, String> {
        if request.identity() == STD_UI_WINDOW_RUNTIME_CONTRACT {
            return self.execute_window(request);
        }
        match self.fallback.as_mut() {
            Some(fallback) => fallback.external_contract(request),
            None => Err(ADAPTER_FAILURE.to_owned()),
        }
    }
}

impl QtRuntimeExecutor {
    fn execute_window(
        &mut self,
        request: ClientExternalContractRequest,
    ) -> Result<RuntimeValue, String> {
        if request.identity() != STD_UI_WINDOW_RUNTIME_CONTRACT {
            return Err(ADAPTER_FAILURE.to_owned());
        }

        let arguments = request.arguments();
        if arguments.len() != 2
            || arguments[0].0 != STD_UI_WINDOW_TITLE_PARAMETER_ID
            || arguments[1].0 != STD_UI_WINDOW_CONTENT_PARAMETER_ID
        {
            return Err(ADAPTER_FAILURE.to_owned());
        }

        let RuntimeValue::Text(title) = &arguments[0].1 else {
            return Err(ADAPTER_FAILURE.to_owned());
        };
        let RuntimeValue::Opaque(content) = &arguments[1].1 else {
            return Err(ADAPTER_FAILURE.to_owned());
        };
        if content.opaque_type() != STD_UI_TYPE_ID {
            return Err(ADAPTER_FAILURE.to_owned());
        }
        self.show_window(title, content.canonical_payload())
            .map_err(|_| ADAPTER_FAILURE.to_owned())?;

        // Preserve the caller's typed UI value, including its exact owned
        // canonical frame, rather than returning a reconstructed projection.
        Ok(arguments[1].1.clone())
    }

    /// Lowers one canonical UI frame and displays it as a new surface.
    ///
    /// The frame is the `ORNA-UI/1` value returned by a CLIENT function. The
    /// host receives the surface handle so it can pump, inspect, and retire
    /// the surface without constructing ABI batches itself.
    pub fn show_window(
        &mut self,
        title: &str,
        canonical_ui_frame: &[u8],
    ) -> Result<AbiSurfaceHandle, RuntimeSessionError> {
        let body = decode_ui_frame(canonical_ui_frame)
            .map_err(|_| RuntimeSessionError::InvalidArgument)?;
        let (batch, next_alias, action_bindings) = lower_ui_content(&body, title, self.next_alias)
            .map_err(|_| RuntimeSessionError::InvalidArgument)?;
        self.next_alias = next_alias;

        let surface = self.session.create_surface(title)?;
        if let Err(error) = self.session.apply_batch(surface, &batch) {
            let _ = self.session.destroy_surface(surface);
            return Err(error);
        }
        if let Err(error) = self.session.set_surface_visible(surface, true) {
            let _ = self.session.destroy_surface(surface);
            return Err(error);
        }
        self.active_surfaces.insert(surface);
        for (action, action_id) in action_bindings {
            self.action_bindings
                .insert(action, RuntimeActionBinding { surface, action_id });
        }
        Ok(surface)
    }
}
fn reconcile_surface_events(
    active_surfaces: &mut HashSet<AbiSurfaceHandle>,
    action_bindings: &mut HashMap<AbiActionHandle, RuntimeActionBinding>,
    events: &[RuntimeEventSnapshot],
) {
    for event in events {
        if let RuntimeEventSnapshot::SurfaceClosed(closed) = event {
            active_surfaces.remove(&closed.surface);
            action_bindings.retain(|_, binding| binding.surface != closed.surface);
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AdapterInputError;

fn decode_ui_frame(payload: &[u8]) -> Result<Value, AdapterInputError> {
    if payload.len() > CLIENT_MAX_RUNTIME_VALUE_BYTES {
        return Err(AdapterInputError);
    }
    let magic = UI_MAGIC.as_bytes();
    let prefix_len = magic.len().checked_add(4).ok_or(AdapterInputError)?;
    if payload.len() < prefix_len || !payload.starts_with(magic) {
        return Err(AdapterInputError);
    }
    let body_len = usize::try_from(u32::from_be_bytes(
        payload[magic.len()..prefix_len]
            .try_into()
            .map_err(|_| AdapterInputError)?,
    ))
    .map_err(|_| AdapterInputError)?;
    let body_end = prefix_len.checked_add(body_len).ok_or(AdapterInputError)?;
    if body_len > CLIENT_MAX_RUNTIME_VALUE_BYTES || body_end != payload.len() {
        return Err(AdapterInputError);
    }
    let body = &payload[prefix_len..body_end];
    let value = serde_json::from_slice(body).map_err(|_| AdapterInputError)?;
    // Re-encoding is both the canonical-form check and duplicate-object-key
    // rejection: serde_json's canonical map representation cannot equal a
    // body containing duplicate keys or trailing JSON whitespace.
    let canonical = serde_json::to_vec(&value).map_err(|_| AdapterInputError)?;
    if canonical != body {
        return Err(AdapterInputError);
    }
    Ok(value)
}

struct LoweringState {
    next_alias: u64,
    node_count: usize,
    value_count: usize,
    action_ids: HashSet<String>,
    action_bindings: Vec<(AbiActionHandle, String)>,
}

impl LoweringState {
    fn new(next_alias: u64) -> Result<Self, AdapterInputError> {
        if next_alias == 0 {
            return Err(AdapterInputError);
        }
        Ok(Self {
            next_alias,
            node_count: 1, // The adapter-owned window root.
            value_count: 0,
            action_ids: HashSet::new(),
            action_bindings: Vec::new(),
        })
    }

    fn next_alias(&mut self) -> Result<u64, AdapterInputError> {
        let alias = self.next_alias;
        if alias == 0 {
            return Err(AdapterInputError);
        }
        self.next_alias = self.next_alias.checked_add(1).ok_or(AdapterInputError)?;
        Ok(alias)
    }

    fn count_value(&mut self) -> Result<(), AdapterInputError> {
        self.value_count = self.value_count.checked_add(1).ok_or(AdapterInputError)?;
        if self.value_count > MAX_UI_NODES {
            return Err(AdapterInputError);
        }
        Ok(())
    }

    fn count_node(&mut self) -> Result<(), AdapterInputError> {
        self.node_count = self.node_count.checked_add(1).ok_or(AdapterInputError)?;
        if self.node_count > MAX_UI_NODES {
            return Err(AdapterInputError);
        }
        Ok(())
    }

    /// Flattens `empty` and `fragment` values without recursive calls.
    fn flatten<'a>(&mut self, values: &'a [Value]) -> Result<Vec<&'a Value>, AdapterInputError> {
        let mut pending = values.iter().rev().collect::<Vec<_>>();
        let mut output = Vec::new();
        while let Some(value) = pending.pop() {
            self.count_value()?;
            let object = value.as_object().ok_or(AdapterInputError)?;
            match object.get("kind").and_then(Value::as_str) {
                Some("empty") if object.len() == 1 => {}
                Some("fragment") if object.len() == 2 => {
                    let children = object
                        .get("children")
                        .and_then(Value::as_array)
                        .ok_or(AdapterInputError)?;
                    pending.extend(children.iter().rev());
                }
                Some("node") => {
                    self.count_node()?;
                    output.push(value);
                }
                _ => return Err(AdapterInputError),
            }
        }
        Ok(output)
    }
}

struct PendingMount<'a> {
    value: &'a Value,
    parent: AbiNodeHandle,
    slot: &'static str,
    ordinal: usize,
}
struct ParsedAction {
    action_id: String,
    event_name: String,
    input_type: String,
}

struct ParsedNode<'a> {
    contract_name: &'static str,
    explicit_key: RuntimeValueInput,
    properties: Vec<(String, RuntimeValueInput)>,
    actions: Vec<ParsedAction>,
    content_children: Option<&'a [Value]>,
}

/// The lowered batch and callback identities are one atomic UI projection result.
type LoweredUiContent = (RuntimeUiBatch, u64, Vec<(AbiActionHandle, String)>);

fn lower_ui_content(
    content: &Value,
    title: &str,
    first_alias: u64,
) -> Result<LoweredUiContent, AdapterInputError> {
    if title.len() > CLIENT_MAX_RUNTIME_TEXT_BYTES {
        return Err(AdapterInputError);
    }
    let mut state = LoweringState::new(first_alias)?;
    let root = state.next_alias()?;
    let mut batch = RuntimeUiBatch::new(UI_SEMANTIC_REVISION);
    push_operation(
        &mut batch,
        RuntimeUiOperation::mount_node(
            root,
            0,
            ROOT_SLOT,
            0,
            WINDOW_CONTRACT,
            1,
            0,
            RuntimeValueInput::empty(),
        ),
    )?;
    push_operation(
        &mut batch,
        RuntimeUiOperation::set_property(
            root,
            "title",
            RuntimeValueInput::new(0, TEXT_TYPE, title.as_bytes().to_vec()),
        ),
    )?;

    let roots = state.flatten(std::slice::from_ref(content))?;
    let mut pending = Vec::with_capacity(roots.len());
    for (ordinal, value) in roots.into_iter().enumerate().rev() {
        pending.push(PendingMount {
            value,
            parent: root,
            slot: CONTENT_SLOT,
            ordinal,
        });
    }

    while let Some(PendingMount {
        value,
        parent,
        slot,
        ordinal,
    }) = pending.pop()
    {
        let parsed = parse_node(value, parent, &mut state)?;
        let node = state.next_alias()?;
        push_operation(
            &mut batch,
            RuntimeUiOperation::mount_node(
                node,
                parent,
                slot,
                ordinal,
                parsed.contract_name,
                1,
                0,
                parsed.explicit_key,
            ),
        )?;
        for (property, value) in parsed.properties {
            push_operation(
                &mut batch,
                RuntimeUiOperation::set_property(node, property, value),
            )?;
        }
        for action in parsed.actions {
            let action_alias = state.next_alias()?;
            push_operation(
                &mut batch,
                RuntimeUiOperation::bind_action(
                    node,
                    action.event_name,
                    action_alias,
                    action.input_type,
                ),
            )?;
            state.action_bindings.push((action_alias, action.action_id));
        }
        if let Some(children) = parsed.content_children {
            let children = state.flatten(children)?;
            for (ordinal, value) in children.into_iter().enumerate().rev() {
                pending.push(PendingMount {
                    value,
                    parent: node,
                    slot: CONTENT_SLOT,
                    ordinal,
                });
            }
        }
    }

    Ok((batch, state.next_alias, state.action_bindings))
}

fn push_operation(
    batch: &mut RuntimeUiBatch,
    operation: RuntimeUiOperation,
) -> Result<(), AdapterInputError> {
    if batch.operations.len() >= CLIENT_MAX_RUNTIME_BATCH_OPERATIONS {
        return Err(AdapterInputError);
    }
    batch.push(operation).map_err(|_| AdapterInputError)
}

fn parse_node<'a>(
    value: &'a Value,
    parent: AbiNodeHandle,
    state: &mut LoweringState,
) -> Result<ParsedNode<'a>, AdapterInputError> {
    let object = value.as_object().ok_or(AdapterInputError)?;
    if !(5..=9).contains(&object.len())
        || object.keys().any(|key| {
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
        })
        || object.get("kind").and_then(Value::as_str) != Some("node")
    {
        return Err(AdapterInputError);
    }

    validate_optional_id(object.get("call_site_id"))?;
    validate_optional_id(object.get("function_instance_id"))?;
    validate_source_origin(object.get("source_origin"))?;

    let (contract_name, major, minor) =
        parse_contract(object.get("contract").ok_or(AdapterInputError)?)?;
    if major != 1 || minor != 0 || contract_name == WINDOW_CONTRACT || parent == 0 {
        return Err(AdapterInputError);
    }

    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .ok_or(AdapterInputError)?;
    let slots = object
        .get("slots")
        .and_then(Value::as_object)
        .ok_or(AdapterInputError)?;
    let actions = object
        .get("actions")
        .and_then(Value::as_object)
        .ok_or(AdapterInputError)?;

    let explicit_key = lower_key(object.get("key"))?;
    let mut lowered_properties = Vec::with_capacity(properties.len());
    for (property, value) in properties {
        let property = bounded_name(property)?;
        lowered_properties.push((
            property.to_owned(),
            lower_property(contract_name, property, value)?,
        ));
    }

    if !actions.is_empty() && contract_name != BUTTON_CONTRACT {
        return Err(AdapterInputError);
    }
    let mut lowered_actions = Vec::with_capacity(actions.len());
    for (event_name, value) in actions {
        let event_name = bounded_name(event_name)?.to_owned();
        let action = value.as_object().ok_or(AdapterInputError)?;
        if action.len() < 2 {
            return Err(AdapterInputError);
        }
        let action_id = action
            .get("action_id")
            .and_then(Value::as_str)
            .ok_or(AdapterInputError)
            .and_then(bounded_name)?
            .to_owned();
        if !state.action_ids.insert(action_id.clone()) {
            return Err(AdapterInputError);
        }
        let input_type = action
            .get("input_type")
            .and_then(Value::as_str)
            .ok_or(AdapterInputError)
            .and_then(bounded_non_empty)?
            .to_owned();
        if let Some(debug_kind) = action.get("debug_kind")
            && !debug_kind.is_null()
        {
            debug_kind
                .as_str()
                .ok_or(AdapterInputError)
                .and_then(bounded_name)?;
        }
        lowered_actions.push(ParsedAction {
            action_id,
            event_name,
            input_type,
        });
    }

    let is_container = matches!(
        contract_name,
        WINDOW_CONTRACT | PANEL_CONTRACT | ROW_CONTRACT | COLUMN_CONTRACT | TABS_CONTRACT
    );
    if slots.keys().any(|slot| slot != CONTENT_SLOT) || (!is_container && !slots.is_empty()) {
        return Err(AdapterInputError);
    }
    let content_children = if is_container {
        match slots.get(CONTENT_SLOT) {
            Some(value) => Some(value.as_array().ok_or(AdapterInputError)?.as_slice()),
            None => None,
        }
    } else {
        None
    };

    Ok(ParsedNode {
        contract_name,
        explicit_key,
        properties: lowered_properties,
        actions: lowered_actions,
        content_children,
    })
}

fn parse_contract(value: &Value) -> Result<(&'static str, u32, u32), AdapterInputError> {
    let object = value.as_object().ok_or(AdapterInputError)?;
    if object.len() != 3
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "id" | "name" | "version"))
    {
        return Err(AdapterInputError);
    }
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or(AdapterInputError)
        .and_then(bounded_non_empty)?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or(AdapterInputError)
        .and_then(bounded_non_empty)?;
    if id != name {
        return Err(AdapterInputError);
    }
    let contract_name = match name {
        WINDOW_CONTRACT => WINDOW_CONTRACT,
        TEXT_CONTRACT => TEXT_CONTRACT,
        BUTTON_CONTRACT => BUTTON_CONTRACT,
        PANEL_CONTRACT => PANEL_CONTRACT,
        ROW_CONTRACT => ROW_CONTRACT,
        COLUMN_CONTRACT => COLUMN_CONTRACT,
        TEXT_INPUT_CONTRACT => TEXT_INPUT_CONTRACT,
        TABS_CONTRACT => TABS_CONTRACT,
        _ => return Err(AdapterInputError),
    };
    let version = object
        .get("version")
        .and_then(Value::as_str)
        .ok_or(AdapterInputError)
        .and_then(bounded_non_empty)?;
    let (major, minor) = parse_version(version)?;
    Ok((contract_name, major, minor))
}

fn parse_version(version: &str) -> Result<(u32, u32), AdapterInputError> {
    let (major, minor) = version.split_once('.').ok_or(AdapterInputError)?;
    if major.is_empty()
        || minor.is_empty()
        || !major.bytes().all(|byte| byte.is_ascii_digit())
        || !minor.bytes().all(|byte| byte.is_ascii_digit())
        || major.len() > 1 && major.starts_with('0')
        || minor.len() > 1 && minor.starts_with('0')
    {
        return Err(AdapterInputError);
    }
    let major = major.parse::<u32>().map_err(|_| AdapterInputError)?;
    let minor = minor.parse::<u32>().map_err(|_| AdapterInputError)?;
    Ok((major, minor))
}

fn lower_property(
    contract: &str,
    property: &str,
    value: &Value,
) -> Result<RuntimeValueInput, AdapterInputError> {
    let object = value.as_object().ok_or(AdapterInputError)?;
    if object.len() != 2
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "type" | "value"))
    {
        return Err(AdapterInputError);
    }
    let type_name = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or(AdapterInputError)?;
    match (contract, property, type_name) {
        (WINDOW_CONTRACT, "title", type_name)
        | (TEXT_CONTRACT, "text", type_name)
        | (BUTTON_CONTRACT, "label", type_name)
        | (TEXT_INPUT_CONTRACT, "text", type_name)
        | (TEXT_INPUT_CONTRACT, "placeholder", type_name)
            if is_text_type(type_name) =>
        {
            let text = object
                .get("value")
                .and_then(Value::as_str)
                .ok_or(AdapterInputError)?;
            let bytes = lower_text_bytes(type_name, text)?;
            Ok(RuntimeValueInput::new(0, TEXT_TYPE, bytes))
        }
        (BUTTON_CONTRACT, "enabled", type_name) | (TEXT_INPUT_CONTRACT, "enabled", type_name)
            if is_boolean_type(type_name) =>
        {
            let bytes =
                lower_boolean_bytes(type_name, object.get("value").ok_or(AdapterInputError)?)?;
            Ok(RuntimeValueInput::new(0, BOOLEAN_TYPE, bytes))
        }
        _ => Err(AdapterInputError),
    }
}

fn lower_key(value: Option<&Value>) -> Result<RuntimeValueInput, AdapterInputError> {
    let Some(value) = value else {
        return Ok(RuntimeValueInput::empty());
    };
    if value.is_null() {
        return Ok(RuntimeValueInput::empty());
    }
    let (type_name, canonical_encoding) = if let Some(object) = value.as_object() {
        let captured_type = object.get("type").and_then(Value::as_str);
        let captured_value = object.get("value").and_then(Value::as_str);
        if object.len() == 2 && captured_type == Some("") && captured_value == Some("") {
            return Ok(RuntimeValueInput::empty());
        }
        if object.len() == 2 && captured_type == Some(JSON_VALUE_TYPE) && captured_value.is_some() {
            let value = captured_value.ok_or(AdapterInputError)?;
            (
                JSON_VALUE_TYPE.to_owned(),
                decode_hex(value).ok_or(AdapterInputError)?,
            )
        } else {
            (
                JSON_VALUE_TYPE.to_owned(),
                serde_json::to_vec(value).map_err(|_| AdapterInputError)?,
            )
        }
    } else {
        (
            JSON_VALUE_TYPE.to_owned(),
            serde_json::to_vec(value).map_err(|_| AdapterInputError)?,
        )
    };
    if canonical_encoding.is_empty() || canonical_encoding.len() > CLIENT_MAX_RUNTIME_VALUE_BYTES {
        return Err(AdapterInputError);
    }
    Ok(RuntimeValueInput::new(0, type_name, canonical_encoding))
}

fn is_text_type(type_name: &str) -> bool {
    matches!(type_name, TEXT_TYPE | STANDARD_TEXT_TYPE)
}

fn is_boolean_type(type_name: &str) -> bool {
    matches!(type_name, BOOLEAN_TYPE | STANDARD_BOOLEAN_TYPE)
}

fn lower_text_bytes(type_name: &str, value: &str) -> Result<Vec<u8>, AdapterInputError> {
    let bytes = match type_name {
        STANDARD_TEXT_TYPE => value.as_bytes().to_vec(),
        TEXT_TYPE => decode_hex(value).ok_or(AdapterInputError)?,
        _ => return Err(AdapterInputError),
    };
    if bytes.len() > CLIENT_MAX_RUNTIME_VALUE_BYTES {
        return Err(AdapterInputError);
    }
    Ok(bytes)
}

fn lower_boolean_bytes(type_name: &str, value: &Value) -> Result<Vec<u8>, AdapterInputError> {
    if let Some(value) = value.as_bool() {
        return Ok(vec![u8::from(value)]);
    }
    if type_name == BOOLEAN_TYPE
        && let Some(value) = value.as_str()
    {
        return match value {
            "00" => Ok(vec![0]),
            "01" => Ok(vec![1]),
            _ => Err(AdapterInputError),
        };
    }
    Err(AdapterInputError)
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Some(bytes)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn bounded_name(value: &str) -> Result<&str, AdapterInputError> {
    if value.is_empty()
        || value.len() > CLIENT_MAX_RUNTIME_TEXT_BYTES
        || value.chars().any(|character| character.is_control())
    {
        return Err(AdapterInputError);
    }
    Ok(value)
}

fn bounded_non_empty(value: &str) -> Result<&str, AdapterInputError> {
    bounded_name(value)
}

fn validate_optional_id(value: Option<&Value>) -> Result<(), AdapterInputError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    value
        .as_str()
        .ok_or(AdapterInputError)
        .and_then(bounded_name)
        .map(|_| ())
}

fn validate_source_origin(value: Option<&Value>) -> Result<(), AdapterInputError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let object = value.as_object().ok_or(AdapterInputError)?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "source_unit_id" | "start" | "end"))
    {
        return Err(AdapterInputError);
    }
    if let Some(source_unit_id) = object.get("source_unit_id") {
        source_unit_id
            .as_str()
            .ok_or(AdapterInputError)
            .and_then(bounded_name)?;
    }
    for key in ["start", "end"] {
        if let Some(value) = object.get(key) {
            value.as_i64().ok_or(AdapterInputError)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(body: &str) -> Vec<u8> {
        let body = body.as_bytes();
        let mut frame = UI_MAGIC.as_bytes().to_vec();
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }

    #[test]
    fn decodes_only_exact_canonical_frames() {
        let body = r#"{"kind":"empty"}"#;
        let decoded = decode_ui_frame(&frame(body)).expect("canonical UI frame");
        assert_eq!(decoded, serde_json::json!({"kind": "empty"}));

        let mut trailing = frame(body);
        trailing.push(b' ');
        assert!(decode_ui_frame(&trailing).is_err());
        assert!(decode_ui_frame(b"ORNA-UI/1 ").is_err());
        assert!(decode_ui_frame(b"not-a-ui-frame").is_err());
    }

    #[test]
    fn rejects_duplicate_json_keys_through_canonical_check() {
        let body = r#"{"kind":"empty","kind":"empty"}"#;
        assert!(decode_ui_frame(&frame(body)).is_err());
    }

    #[test]
    fn lowers_fragment_and_provider_properties_iteratively() {
        let content = serde_json::json!({
            "kind": "fragment",
            "children": [
                {
                    "kind": "node",
                    "contract": {"id": "std.ui.text", "name": "std.ui.text", "version": "1.0"},
                    "properties": {"text": {"type": STANDARD_TEXT_TYPE, "value": "hello"}},
                    "slots": {},
                    "actions": {}
                },
                {
                    "kind": "node",
                    "contract": {"id": "std.ui.button", "name": "std.ui.button", "version": "1.0"},
                    "properties": {
                        "label": {"type": STANDARD_TEXT_TYPE, "value": "go"},
                        "enabled": {"type": STANDARD_BOOLEAN_TYPE, "value": true}
                    },
                    "slots": {},
                    "actions": {
                        "clicked": {"action_id": "go", "input_type": TEXT_TYPE, "trace": true}
                    }
                }
            ]
        });
        let (batch, next_alias, action_bindings) =
            lower_ui_content(&content, "Title", 1).expect("lowered UI");
        assert_eq!(next_alias, 5);
        assert_eq!(action_bindings, vec![(4, "go".to_owned())]);
        assert!(matches!(
            &batch.operations[0],
            RuntimeUiOperation::MountNode {
                node: 1,
                parent: 0,
                slot,
                contract_name,
                ..
            } if slot == ROOT_SLOT && contract_name == WINDOW_CONTRACT
        ));
        assert!(matches!(
            &batch.operations[1],
            RuntimeUiOperation::SetProperty { node: 1, property, value }
                if property == "title"
                    && value.type_name == TEXT_TYPE
                    && value.canonical_encoding == b"Title"
        ));
        assert!(matches!(
            &batch.operations[2],
            RuntimeUiOperation::MountNode {
                node: 2,
                parent: 1,
                ordinal: 0,
                contract_name,
                ..
            } if contract_name == TEXT_CONTRACT
        ));
        assert!(matches!(
            &batch.operations[4],
            RuntimeUiOperation::MountNode {
                node: 3,
                parent: 1,
                ordinal: 1,
                contract_name,
                ..
            } if contract_name == BUTTON_CONTRACT
        ));
        assert!(matches!(
            &batch.operations[7],
            RuntimeUiOperation::BindAction {
                node: 3,
                action: 4,
                event_name,
                input_type
            } if event_name == "clicked" && input_type == TEXT_TYPE
        ));
    }

    #[test]
    fn lowers_schema_keys_and_runtime_canonical_values() {
        let content = serde_json::json!({
            "kind": "node",
            "contract": {"id": TEXT_CONTRACT, "name": TEXT_CONTRACT, "version": "1.0"},
            "key": {"id": 1},
            "properties": {
                "text": {"type": STANDARD_TEXT_TYPE, "value": "deadbeef"}
            },
            "slots": {},
            "actions": {}
        });
        let (batch, _, _) = lower_ui_content(&content, "Title", 1).expect("lowered keyed UI");
        let RuntimeUiOperation::MountNode { explicit_key, .. } = &batch.operations[2] else {
            panic!("expected the keyed content mount");
        };
        assert_eq!(explicit_key.handle, 0);
        assert_eq!(explicit_key.type_name, JSON_VALUE_TYPE);
        assert_eq!(explicit_key.canonical_encoding, br#"{"id":1}"#);
        assert_eq!(
            lower_property(
                TEXT_CONTRACT,
                "text",
                &serde_json::json!({"type": TEXT_TYPE, "value": "6869"})
            )
            .expect("captured text property")
            .canonical_encoding,
            b"hi"
        );
        assert_eq!(
            lower_property(
                BUTTON_CONTRACT,
                "enabled",
                &serde_json::json!({"type": BOOLEAN_TYPE, "value": "01"})
            )
            .expect("captured Boolean property")
            .canonical_encoding,
            [1]
        );
        let captured_key =
            serde_json::json!({"type": JSON_VALUE_TYPE, "value": "7b226964223a317d"});
        let lowered = lower_key(Some(&captured_key)).expect("captured key");
        assert_eq!(lowered.handle, 0);
        assert_eq!(lowered.canonical_encoding, br#"{"id":1}"#);
    }

    #[test]
    fn rejects_unknown_contract_property_type_and_slot() {
        let mut content = serde_json::json!({
            "kind": "node",
            "contract": {"id": "std.ui.text", "name": "std.ui.text", "version": "1.0"},
            "properties": {"text": {"type": "std.boolean", "value": true}},
            "slots": {},
            "actions": {}
        });
        assert!(lower_ui_content(&content, "Title", 1).is_err());

        content["contract"]["name"] = Value::String("std.ui.unknown".to_owned());
        content["contract"]["id"] = Value::String("std.ui.unknown".to_owned());
        content["properties"] = serde_json::json!({});
        assert!(lower_ui_content(&content, "Title", 1).is_err());

        content["contract"] =
            serde_json::json!({"id": "std.ui.text", "name": "std.ui.text", "version": "1.0"});
        content["slots"] = serde_json::json!({"other": []});
        assert!(lower_ui_content(&content, "Title", 1).is_err());
    }
    #[test]
    fn parses_only_canonical_provider_versions() {
        assert_eq!(parse_version("1.0"), Ok((1, 0)));
        assert!(parse_version("1").is_err());
        assert!(parse_version("01.0").is_err());
        assert!(parse_version("1.00").is_err());
        assert!(parse_version("+1.0").is_err());
    }
    #[test]
    fn reconciles_surface_closed_events_for_adapter_bookkeeping() {
        let closed_surface = 7;
        let retained_surface = 8;
        let closed_action = 11;
        let retained_action = 12;
        let mut active_surfaces = HashSet::from([closed_surface, retained_surface]);
        let mut action_bindings = HashMap::from([
            (
                closed_action,
                RuntimeActionBinding {
                    surface: closed_surface,
                    action_id: "closed".to_owned(),
                },
            ),
            (
                retained_action,
                RuntimeActionBinding {
                    surface: retained_surface,
                    action_id: "retained".to_owned(),
                },
            ),
        ]);
        let events = [RuntimeEventSnapshot::SurfaceClosed(
            crate::runtime_loader::RuntimeSurfaceClosedEventSnapshot {
                surface: closed_surface,
            },
        )];

        reconcile_surface_events(&mut active_surfaces, &mut action_bindings, &events);

        assert_eq!(active_surfaces, HashSet::from([retained_surface]));
        assert!(!action_bindings.contains_key(&closed_action));
        assert!(action_bindings.contains_key(&retained_action));
        assert_eq!(events.len(), 1);
    }
}
