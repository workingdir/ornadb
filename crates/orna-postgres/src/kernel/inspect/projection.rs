//! Immutable projections derived during INSPECT capture.

use super::*;

/// Builds one immutable inspection epoch from the sealed dispatch facts.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_inspect_epoch(
    active: &ActiveDatabaseRevision,
    invocation: InvocationId,
    options: InspectSnapshotOptions,
    owner: PrincipalId,
    root_target: FunctionId,
    outcome: InspectOutcomeKind,
    events: &InvocationEventBatch,
    client_offer: Option<&InvocationClientOffer>,
    state_cells: Vec<StateCellRow>,
    security_decisions: Vec<SecurityDecisionRow>,
    output_requirement: Option<&InvocationOutputRequirement>,
) -> Result<InspectSnapshotEpoch, PostgresKernelError> {
    let mut value_count = 0_u64;
    let mut schema = None;
    for record in events.records() {
        if let InvocationEventBody::ValueBatch {
            schema: batch_schema,
            values,
        } = record.event().body()
        {
            value_count = values.len() as u64;
            schema = batch_schema.clone();
        }
    }
    let (phase, duration_nanoseconds) = inspect_epoch_metadata(outcome, events);
    let result = if value_count == 0 {
        InspectResultSummary::NoValues
    } else {
        InspectResultSummary::ValueBatch { value_count }
    };
    let summary =
        InspectSnapshotSummary::new(events.records().len() as u64, result, duration_nanoseconds)
            .map_err(PostgresKernelError::Inspect)?;
    let node = InvocationNodeRow::new(
        invocation,
        None,
        InspectInvocationNodeKind::Root,
        phase,
        root_target,
        0,
    )
    .map_err(PostgresKernelError::Inspect)?;
    let call = CallRow::new(
        invocation,
        schema,
        value_count,
        duration_nanoseconds.unwrap_or(0),
    )
    .map_err(PostgresKernelError::Inspect)?;
    let runtime_bindings = client_offer
        .map(runtime_bindings_from_offer)
        .transpose()?
        .unwrap_or_default();
    let resources = resource_rows_from_capture(
        true,
        active.catalogue_hash_context().standard().is_some(),
        client_offer.is_some_and(|offer| !offer.runtime_offers().is_empty()),
    );
    let ui_nodes = if client_offer.is_some() {
        ui_nodes_from_events(invocation, root_target, events)?
    } else {
        Vec::new()
    };
    let presentation_candidates = presentation_candidates_from_capture(
        invocation,
        outcome,
        events,
        client_offer,
        output_requirement,
    )?;
    InspectSnapshotEpoch::new(
        InspectEpochId::new(),
        invocation,
        active.pair().source(),
        active.pair().catalogue(),
        owner,
        SystemTime::now(),
        root_target,
        outcome,
        summary,
        &options,
        vec![node],
        vec![call],
        resources,
        state_cells,
        ui_nodes,
        presentation_candidates,
        runtime_bindings,
        security_decisions,
    )
    .map_err(PostgresKernelError::Inspect)
}

fn resource_rows_from_capture(
    state_loaded: bool,
    standard_available: bool,
    runtime_available: bool,
) -> Vec<ResourceRow> {
    let mut rows = Vec::with_capacity(4);
    if state_loaded {
        rows.push(ResourceRow::new(
            orna_core::inspect::InspectResourceKind::State,
            orna_core::inspect::InspectResourceStatus::Active,
        ));
    }
    rows.push(ResourceRow::new(
        orna_core::inspect::InspectResourceKind::Catalog,
        orna_core::inspect::InspectResourceStatus::Active,
    ));
    if standard_available {
        rows.push(ResourceRow::new(
            orna_core::inspect::InspectResourceKind::Standard,
            orna_core::inspect::InspectResourceStatus::Active,
        ));
    }
    if runtime_available {
        rows.push(ResourceRow::new(
            orna_core::inspect::InspectResourceKind::Runtime,
            orna_core::inspect::InspectResourceStatus::Active,
        ));
    }
    rows
}

fn inspect_capture_invariant(invocation: InvocationId, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: INSPECT_SNAPSHOT_RELATION,
        record: invocation.canonical(),
        rule,
    }
}

fn ui_nodes_from_events(
    invocation: InvocationId,
    root_target: FunctionId,
    events: &InvocationEventBatch,
) -> Result<Vec<UiNodeRow>, PostgresKernelError> {
    let mut rows = Vec::new();
    for record in events.records() {
        let InvocationEventBody::ValueBatch { values, .. } = record.event().body() else {
            continue;
        };
        for value in values {
            let RuntimeValue::Opaque(value) = value.value() else {
                continue;
            };
            if value.opaque_type() != STD_UI_TYPE_ID {
                continue;
            }
            append_ui_nodes_from_payload(
                invocation,
                root_target,
                value.canonical_payload(),
                &mut rows,
            )?;
        }
    }
    Ok(rows)
}

#[cfg(test)]
fn ui_nodes_from_payload(
    invocation: InvocationId,
    root_target: FunctionId,
    payload: &[u8],
) -> Result<Vec<UiNodeRow>, PostgresKernelError> {
    let mut rows = Vec::new();
    append_ui_nodes_from_payload(invocation, root_target, payload, &mut rows)?;
    Ok(rows)
}

fn append_ui_nodes_from_payload(
    invocation: InvocationId,
    root_target: FunctionId,
    payload: &[u8],
    rows: &mut Vec<UiNodeRow>,
) -> Result<(), PostgresKernelError> {
    const UI_MAGIC: &[u8] = b"ORNA-UI/1 ";
    let prefix_length = UI_MAGIC.len().checked_add(4).expect("UI prefix is bounded");
    if payload.len() < prefix_length || !payload.starts_with(UI_MAGIC) {
        return Err(inspect_capture_invariant(
            invocation,
            "captured UI value must use the canonical frame",
        ));
    }
    let body_length = u32::from_be_bytes(
        payload[UI_MAGIC.len()..prefix_length]
            .try_into()
            .expect("the UI length prefix is exactly four bytes"),
    ) as usize;
    let body_end = prefix_length.checked_add(body_length).ok_or_else(|| {
        inspect_capture_invariant(invocation, "captured UI value frame length overflowed")
    })?;
    if body_length > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH || body_end != payload.len() {
        return Err(inspect_capture_invariant(
            invocation,
            "captured UI value frame length is invalid",
        ));
    }
    let body = &payload[prefix_length..body_end];
    let value = serde_json::from_slice::<serde_json::Value>(body).map_err(|_| {
        inspect_capture_invariant(invocation, "captured UI value body is not canonical JSON")
    })?;
    let canonical_body = serde_json::to_vec(&value).map_err(|_| {
        inspect_capture_invariant(invocation, "captured UI value body is not canonical JSON")
    })?;
    if canonical_body != body {
        return Err(inspect_capture_invariant(
            invocation,
            "captured UI value body is not canonical JSON",
        ));
    }

    let mut pending = vec![&value];
    let mut node_count = 0_usize;
    while let Some(value) = pending.pop() {
        node_count = node_count.checked_add(1).ok_or_else(|| {
            inspect_capture_invariant(invocation, "captured UI node count overflowed")
        })?;
        if node_count > MAX_RUNTIME_VALUE_NODES {
            return Err(inspect_capture_invariant(
                invocation,
                "captured UI node count exceeds the runtime value bound",
            ));
        }
        let object = value.as_object().ok_or_else(|| {
            inspect_capture_invariant(invocation, "captured UI node has invalid shape")
        })?;
        match object.get("kind").and_then(serde_json::Value::as_str) {
            Some("empty") if object.len() == 1 => {}
            Some("fragment") => {
                if object.len() != 2
                    || object
                        .keys()
                        .any(|key| key.as_str() != "kind" && key.as_str() != "children")
                {
                    return Err(inspect_capture_invariant(
                        invocation,
                        "captured UI fragment has invalid shape",
                    ));
                }
                let children = object
                    .get("children")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| {
                        inspect_capture_invariant(
                            invocation,
                            "captured UI fragment children are invalid",
                        )
                    })?;
                for child in children.iter().rev() {
                    pending.push(child);
                }
            }
            Some("node") => {
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
                {
                    return Err(inspect_capture_invariant(
                        invocation,
                        "captured UI node has invalid shape",
                    ));
                }
                let contract = object
                    .get("contract")
                    .and_then(serde_json::Value::as_object)
                    .ok_or_else(|| {
                        inspect_capture_invariant(
                            invocation,
                            "captured UI node contract is invalid",
                        )
                    })?;
                if contract.len() != 3
                    || contract
                        .keys()
                        .any(|key| !matches!(key.as_str(), "id" | "name" | "version"))
                {
                    return Err(inspect_capture_invariant(
                        invocation,
                        "captured UI node contract is invalid",
                    ));
                }
                let contract_id = contract
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        inspect_capture_invariant(
                            invocation,
                            "captured UI node contract identity is invalid",
                        )
                    })?;
                let contract_name = contract
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        inspect_capture_invariant(
                            invocation,
                            "captured UI node contract name is invalid",
                        )
                    })?;
                let contract_version = contract
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        inspect_capture_invariant(
                            invocation,
                            "captured UI node contract version is invalid",
                        )
                    })?;
                if !valid_ui_label(contract_id)
                    || !valid_ui_label(contract_name)
                    || !valid_ui_label(contract_version)
                    || contract_id != contract_name
                {
                    return Err(inspect_capture_invariant(
                        invocation,
                        "captured UI node contract identity is invalid",
                    ));
                }

                let call_site = match object.get("call_site_id") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(call_site)) => {
                        if call_site.is_empty() {
                            return Err(inspect_capture_invariant(
                                invocation,
                                "captured UI node call-site identity is empty",
                            ));
                        }
                        let call_site_id = CallSiteId::from_canonical(call_site).map_err(|_| {
                            inspect_capture_invariant(
                                invocation,
                                "captured UI node call-site identity is not canonical",
                            )
                        })?;
                        if call_site_id.to_bytes() == [0; 16] {
                            return Err(inspect_capture_invariant(
                                invocation,
                                "captured UI node call-site identity is zero",
                            ));
                        }
                        Some(call_site_id.canonical())
                    }
                    Some(_) => {
                        return Err(inspect_capture_invariant(
                            invocation,
                            "captured UI node call-site identity is invalid",
                        ));
                    }
                };
                if let Some(instance) = object.get("function_instance_id") {
                    match instance {
                        serde_json::Value::Null => {}
                        serde_json::Value::String(instance) if valid_ui_label(instance) => {}
                        _ => {
                            return Err(inspect_capture_invariant(
                                invocation,
                                "captured UI node function instance identity is invalid",
                            ));
                        }
                    }
                }
                object
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
                    .ok_or_else(|| {
                        inspect_capture_invariant(
                            invocation,
                            "captured UI node properties are invalid",
                        )
                    })?;
                let slots = object
                    .get("slots")
                    .and_then(serde_json::Value::as_object)
                    .ok_or_else(|| {
                        inspect_capture_invariant(invocation, "captured UI node slots are invalid")
                    })?;
                object
                    .get("actions")
                    .and_then(serde_json::Value::as_object)
                    .ok_or_else(|| {
                        inspect_capture_invariant(
                            invocation,
                            "captured UI node actions are invalid",
                        )
                    })?;

                if let Some(call_site) = call_site {
                    if rows.len() >= MAX_INSPECT_CARRIER_ROWS {
                        return Err(inspect_capture_invariant(
                            invocation,
                            "captured UI row count exceeds the inspect bound",
                        ));
                    }
                    rows.push(
                        UiNodeRow::new(root_target, call_site, contract_id.to_owned()).map_err(
                            |_| {
                                inspect_capture_invariant(
                                    invocation,
                                    "captured UI node row is not canonical",
                                )
                            },
                        )?,
                    );
                }
                for children in slots.values().rev() {
                    let children = children.as_array().ok_or_else(|| {
                        inspect_capture_invariant(
                            invocation,
                            "captured UI node slot children are invalid",
                        )
                    })?;
                    for child in children.iter().rev() {
                        pending.push(child);
                    }
                }
            }
            _ => {
                return Err(inspect_capture_invariant(
                    invocation,
                    "captured UI value has an invalid node kind",
                ));
            }
        }
    }
    Ok(())
}

fn valid_ui_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OPAQUE_CODEC_PAYLOAD_LENGTH
        && value.chars().all(|character| !character.is_control())
}

fn presentation_candidates_from_capture(
    invocation: InvocationId,
    outcome: InspectOutcomeKind,
    events: &InvocationEventBatch,
    client_offer: Option<&InvocationClientOffer>,
    output_requirement: Option<&InvocationOutputRequirement>,
) -> Result<Vec<PresentationCandidateRow>, PostgresKernelError> {
    let Some(output_requirement) = output_requirement else {
        return Ok(Vec::new());
    };
    if outcome != InspectOutcomeKind::Allowed {
        return Ok(Vec::new());
    }
    let Some(final_value) = final_event_value(events) else {
        return Err(inspect_capture_invariant(
            invocation,
            "captured output requirement must have a final event value",
        ));
    };
    let Some(presenter) = presenter_alias_from_capture(output_requirement, final_value) else {
        return Ok(Vec::new());
    };
    let selected_sink = selected_sink_from_final_value(final_value, client_offer);
    let runtime = selected_sink
        .as_ref()
        .and_then(|sink| selected_runtime_from_offer(client_offer, sink));
    let row = PresentationCandidateRow::new(
        presenter,
        true,
        "accepted by output resolution".to_owned(),
        selected_sink,
        runtime,
    )
    .map_err(|_| {
        inspect_capture_invariant(
            invocation,
            "captured presentation candidate row is not canonical",
        )
    })?;
    Ok(vec![row])
}

fn final_event_value(events: &InvocationEventBatch) -> Option<&RuntimeValue> {
    events.records().iter().rev().find_map(|record| {
        let InvocationEventBody::ValueBatch { values, .. } = record.event().body() else {
            return None;
        };
        values.last().map(|value| value.value())
    })
}

fn presenter_alias_from_capture(
    output_requirement: &InvocationOutputRequirement,
    value: &RuntimeValue,
) -> Option<String> {
    if let Some(alias) = output_requirement.alias() {
        return Some(alias.to_owned());
    }
    if let Some(media_type) = output_requirement.media_type() {
        if media_type == "text/plain"
            && matches!(
                value,
                RuntimeValue::Opaque(value)
                    if value.opaque_type() == STD_TERMINAL_DOCUMENT_TYPE_ID
            )
        {
            return Some("table".to_owned());
        }
        let actual_media_type = byte_stream_media_type(value)?;
        if actual_media_type != media_type {
            return None;
        }
        return match actual_media_type {
            "application/json" => Some("json".to_owned()),
            "text/csv" => Some("csv".to_owned()),
            _ => None,
        };
    }
    match value {
        RuntimeValue::Opaque(value) if value.opaque_type() == STD_TERMINAL_DOCUMENT_TYPE_ID => {
            Some("table".to_owned())
        }
        RuntimeValue::Opaque(value) if value.opaque_type() == STD_IO_BYTE_STREAM_TYPE_ID => {
            match byte_stream_media_type_opaque(value) {
                Some("application/json") => Some("json".to_owned()),
                Some("text/csv") => Some("csv".to_owned()),
                _ => None,
            }
        }
        _ => None,
    }
}

fn byte_stream_media_type(value: &RuntimeValue) -> Option<&str> {
    let RuntimeValue::Opaque(value) = value else {
        return None;
    };
    byte_stream_media_type_opaque(value)
}

fn byte_stream_media_type_opaque(value: &OpaqueValue) -> Option<&str> {
    if value.opaque_type() != STD_IO_BYTE_STREAM_TYPE_ID {
        return None;
    }
    let payload = value.canonical_payload();
    let magic = BYTE_STREAM_MAGIC.as_bytes();
    let media_length_start = magic.len();
    let media_length_end = media_length_start.checked_add(4)?;
    if payload.len() < media_length_end || !payload.starts_with(magic) {
        return None;
    }
    let media_length = u32::from_be_bytes(
        payload[media_length_start..media_length_end]
            .try_into()
            .ok()?,
    ) as usize;
    let media_start = media_length_end;
    let media_end = media_start.checked_add(media_length)?;
    let body_length_end = media_end.checked_add(4)?;
    if media_length == 0 || payload.len() < body_length_end {
        return None;
    }
    let body_length =
        u32::from_be_bytes(payload[media_end..body_length_end].try_into().ok()?) as usize;
    let body_end = body_length_end.checked_add(body_length)?;
    if body_end != payload.len() {
        return None;
    }
    std::str::from_utf8(&payload[media_start..media_end]).ok()
}

fn selected_sink_from_final_value(
    value: &RuntimeValue,
    client_offer: Option<&InvocationClientOffer>,
) -> Option<TypeDescriptor> {
    let RuntimeValue::Opaque(value) = value else {
        return None;
    };
    let descriptor = TypeDescriptor::named(value.opaque_type());
    client_offer?
        .sink_offers()
        .iter()
        .find(|offer| offer.descriptor() == &descriptor)
        .map(|offer| offer.descriptor().clone())
}

fn selected_runtime_from_offer(
    client_offer: Option<&InvocationClientOffer>,
    sink: &TypeDescriptor,
) -> Option<String> {
    let mut matching = client_offer?.runtime_offers().iter().filter(|runtime| {
        runtime.trusted()
            && runtime
                .consumed_descriptors()
                .iter()
                .any(|descriptor| descriptor == sink)
    });
    let runtime = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some(runtime.name().to_owned())
}

/// Derives the closed root phase and duration from the outcome and events.
///
/// A duration is meaningful only for an allowed invocation with a completed
/// event. `CallRow` receives the closed zero sentinel when no duration exists
/// because its accepted carrier field is non-optional.
fn inspect_epoch_metadata(
    outcome: InspectOutcomeKind,
    events: &InvocationEventBatch,
) -> (InspectInvocationPhase, Option<u64>) {
    match outcome {
        InspectOutcomeKind::Allowed => events
            .records()
            .iter()
            .rev()
            .find_map(|record| match record.event().body() {
                InvocationEventBody::Completed {
                    duration_nanoseconds,
                } => Some((
                    InspectInvocationPhase::Completed,
                    Some(*duration_nanoseconds),
                )),
                _ => None,
            })
            .unwrap_or((InspectInvocationPhase::Executing, None)),
        InspectOutcomeKind::Denied => (InspectInvocationPhase::Started, None),
        InspectOutcomeKind::Failed => (InspectInvocationPhase::Failed, None),
        InspectOutcomeKind::Cancelled => (InspectInvocationPhase::Cancelled, None),
    }
}

/// Builds the closed runtime-binding rows from one client offer.
fn runtime_bindings_from_offer(
    client_offer: &InvocationClientOffer,
) -> Result<Vec<RuntimeBindingRow>, PostgresKernelError> {
    let mut rows = Vec::with_capacity(client_offer.runtime_offers().len());
    for runtime in client_offer.runtime_offers() {
        let contracts = runtime
            .contracts()
            .iter()
            .map(|contract| {
                (
                    contract.name().to_owned(),
                    contract.version().to_owned(),
                    contract.features().to_vec(),
                )
            })
            .collect();
        // Client offers carry signed ranks; the closed model ranks are
        // unsigned, so a de-prioritised negative rank clamps to rank zero.
        let rank = runtime.preference_rank().max(0) as u32;
        rows.push(
            RuntimeBindingRow::new(
                runtime.name().to_owned(),
                runtime.version().to_owned(),
                runtime.consumed_descriptors().to_vec(),
                contracts,
                runtime.trusted(),
                rank,
            )
            .map_err(PostgresKernelError::Inspect)?,
        );
    }
    Ok(rows)
}

/// Returns the closed durable stream kind of one event body.
pub(super) fn durable_trace_kind(body: &InvocationEventBody) -> Option<&'static str> {
    match body {
        InvocationEventBody::Started { .. } => Some("started"),
        InvocationEventBody::ValueBatch { .. } => Some("value_batch"),
        InvocationEventBody::Completed { .. } => Some("completed"),
        InvocationEventBody::Diagnostic(_) => Some("diagnostic"),
        InvocationEventBody::Failed(_) | InvocationEventBody::Cancelled { .. } => None,
        _ => None,
    }
}

/// Maps one durable stream kind and event body to the closed model payload.
///
/// Kinds the closed v1 model cannot express (for example `inspect_snapshot`)
/// are handled by the caller before this mapping runs.
pub(super) fn model_payload_for(
    kind: &str,
    body: &InvocationEventBody,
) -> Option<InspectTracePayload> {
    match (kind, body) {
        ("started", InvocationEventBody::Started { .. }) => Some(InspectTracePayload::Started),
        ("value_batch", InvocationEventBody::ValueBatch { schema, values }) => {
            Some(InspectTracePayload::ValueBatch {
                schema: schema.clone(),
                values: values.clone(),
            })
        }
        (
            "completed",
            InvocationEventBody::Completed {
                duration_nanoseconds,
            },
        ) => Some(InspectTracePayload::Completed {
            duration_nanoseconds: *duration_nanoseconds,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata_events(body: InvocationEventBody) -> InvocationEventBatch {
        let event =
            orna_core::invocation::InvokeEvent::new(InvocationId::from_bytes([0x11; 16]), 0, body)
                .expect("event body must be valid");
        InvocationEventBatch::new(vec![orna_protocol::InvocationEventRecord::new(1, event)])
            .expect("event batch must be valid")
    }

    #[test]
    fn allowed_completed_event_supplies_duration_and_completed_phase() {
        let events = metadata_events(InvocationEventBody::Completed {
            duration_nanoseconds: 42,
        });
        assert_eq!(
            inspect_epoch_metadata(InspectOutcomeKind::Allowed, &events),
            (InspectInvocationPhase::Completed, Some(42))
        );
    }

    #[test]
    fn allowed_without_completed_event_is_executing_without_duration() {
        let events = metadata_events(InvocationEventBody::Started {
            visible_principal: None,
        });
        assert_eq!(
            inspect_epoch_metadata(InspectOutcomeKind::Allowed, &events),
            (InspectInvocationPhase::Executing, None)
        );
    }

    #[test]
    fn closed_non_completed_outcomes_never_claim_duration() {
        let events = metadata_events(InvocationEventBody::Completed {
            duration_nanoseconds: 42,
        });
        assert_eq!(
            inspect_epoch_metadata(InspectOutcomeKind::Denied, &events),
            (InspectInvocationPhase::Started, None)
        );
        assert_eq!(
            inspect_epoch_metadata(InspectOutcomeKind::Failed, &events),
            (InspectInvocationPhase::Failed, None)
        );
        assert_eq!(
            inspect_epoch_metadata(InspectOutcomeKind::Cancelled, &events),
            (InspectInvocationPhase::Cancelled, None)
        );
    }

    #[test]
    fn populated_resource_projection_is_active_and_bounded() {
        let rows = resource_rows_from_capture(true, true, true);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter().map(ResourceRow::kind).collect::<Vec<_>>(),
            vec![
                orna_core::inspect::InspectResourceKind::State,
                orna_core::inspect::InspectResourceKind::Catalog,
                orna_core::inspect::InspectResourceKind::Standard,
                orna_core::inspect::InspectResourceKind::Runtime,
            ]
        );
        assert!(
            rows.iter()
                .all(|row| { row.status() == orna_core::inspect::InspectResourceStatus::Active })
        );
        assert_eq!(resource_rows_from_capture(true, false, false).len(), 2);
    }

    fn ui_frame(body: serde_json::Value) -> Vec<u8> {
        let body = serde_json::to_vec(&body).expect("UI fixture is serialisable");
        let mut payload = b"ORNA-UI/1 ".to_vec();
        payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        payload.extend_from_slice(&body);
        payload
    }

    fn ui_node(call_site_id: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "kind": "node",
            "contract": {
                "id": "std.ui.window",
                "name": "std.ui.window",
                "version": "1.0"
            },
            "call_site_id": call_site_id,
            "function_instance_id": "instance-not-a-function-id",
            "properties": {},
            "slots": {"content": []},
            "actions": {}
        })
    }

    #[test]
    fn populated_ui_projection_uses_root_function_and_omits_unlabelled_nodes() {
        let invocation = InvocationId::from_bytes([0x11; 16]);
        let root_target = FunctionId::from_bytes([0x22; 16]);
        let call_site = CallSiteId::from_bytes([0x33; 16]).canonical();
        let body = serde_json::json!({
            "kind": "fragment",
            "children": [
                ui_node(serde_json::Value::String(call_site.clone())),
                ui_node(serde_json::Value::Null)
            ]
        });
        let rows = ui_nodes_from_payload(invocation, root_target, &ui_frame(body))
            .expect("canonical UI identities are accepted");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].function(), root_target);
        assert_eq!(rows[0].call_site(), call_site);
        assert_eq!(rows[0].runtime_contract(), "std.ui.window");
    }

    #[test]
    fn malformed_ui_call_site_fails_closed_during_capture() {
        let invocation = InvocationId::from_bytes([0x44; 16]);
        let body = ui_node(serde_json::Value::String("not-a-call-site".to_owned()));
        assert!(matches!(
            ui_nodes_from_payload(
                invocation,
                FunctionId::from_bytes([0x55; 16]),
                &ui_frame(body)
            ),
            Err(PostgresKernelError::DurableInvariant {
                rule: "captured UI node call-site identity is not canonical",
                ..
            })
        ));
    }

    #[test]
    fn populated_presentation_projection_uses_final_event_value() {
        let invocation = InvocationId::from_bytes([0x11; 16]);
        let value = InvokeValue::new(RuntimeValue::Integer(7)).expect("value fixture is valid");
        let events = metadata_events(
            InvocationEventBody::value_batch(None, [value]).expect("value batch is valid"),
        );
        let requirement = InvocationOutputRequirement::new(
            Some("json".to_owned()),
            None,
            None,
            orna_core::invocation::InvocationStreamingRequirement::Unspecified,
        )
        .expect("output requirement is valid");
        let rows = presentation_candidates_from_capture(
            invocation,
            InspectOutcomeKind::Allowed,
            &events,
            None,
            Some(&requirement),
        )
        .expect("accepted presentation is projected");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].presenter(), "json");
        assert!(rows[0].accepted());
        assert_eq!(rows[0].reason(), "accepted by output resolution");
        assert!(rows[0].selected_sink().is_none());
        assert!(rows[0].runtime().is_none());
    }
}
