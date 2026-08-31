//! Installed Inspector execution and carrier projection.

use super::*;
pub(super) const INSPECT_SNAPSHOT_ROW_TAG: u8 = 1;
/// Typed marker used in a classified row field when its classifier is absent.
///
/// Length-delimited text fields use this value in place of their u32 byte
/// length; optional fields use it in place of its presence byte. The value
/// is outside the ordinary field domain and therefore cannot be mistaken for
/// a caller-provided classified value.
pub(super) const INSPECT_REDACTED_FIELD_TAG: u8 = 2;
const INSPECT_REDACTED_TEXT_LENGTH: u32 = u32::MAX;

/// The canonical client-plan header used by installed CLIENT artifacts.
///
/// The server depends on orna-client for execution, but this callback is a
/// separate trust boundary. Keep the narrow root decoder local so the callback
/// can authenticate the installed artifact body without widening the client
/// validator's public API.
const CLIENT_PLAN_MAGIC: &[u8; 8] = b"ORNACP\0\0";
const CLIENT_PLAN_EXPRESSION_VERSION: u32 = 3;
const CLIENT_PLAN_CAPABILITY_VERSION: u32 = 5;
const CLIENT_PLAN_EXPRESSION_OPERATION: u8 = 3;
const CLIENT_PLAN_EXTERNAL_CONTRACT_NODE: u8 = 8;
const CLIENT_PLAN_CAPABILITY_OPERATION: u8 = 5;

fn inspect_render_artifact_is_external(
    revision: &orna_core::revision::FunctionRevisionRecord,
) -> bool {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Client || artifact.format() != "orna.client-plan"
    {
        return false;
    }

    match artifact.version() {
        CLIENT_PLAN_EXPRESSION_VERSION => {
            client_expression_artifact_is_external(artifact.payload())
        }
        CLIENT_PLAN_CAPABILITY_VERSION => {
            let Some((inner_version, inner_payload)) =
                client_capability_inner_artifact(artifact.payload())
            else {
                return false;
            };
            inner_version == CLIENT_PLAN_EXPRESSION_VERSION
                && client_expression_artifact_is_external(inner_payload)
        }
        _ => false,
    }
}

fn client_expression_artifact_is_external(payload: &[u8]) -> bool {
    if payload.len() < 18
        || payload.get(..8) != Some(CLIENT_PLAN_MAGIC.as_slice())
        || u32::from_be_bytes(payload[8..12].try_into().expect("checked header width"))
            != CLIENT_PLAN_EXPRESSION_VERSION
        || payload[12] != CLIENT_PLAN_EXPRESSION_OPERATION
        || payload[13] != CLIENT_PLAN_EXTERNAL_CONTRACT_NODE
    {
        return false;
    }
    let identity_length =
        u32::from_be_bytes(payload[14..18].try_into().expect("checked identity width")) as usize;
    let identity_end = 18usize.saturating_add(identity_length);
    identity_end == payload.len()
        && &payload[18..identity_end] == INSPECT_RENDER_CONTRACT.as_bytes()
}

fn client_capability_inner_artifact(payload: &[u8]) -> Option<(u32, &[u8])> {
    if payload.len() < 21
        || payload.get(..8) != Some(CLIENT_PLAN_MAGIC.as_slice())
        || u32::from_be_bytes(payload[8..12].try_into().ok()?) != CLIENT_PLAN_CAPABILITY_VERSION
        || payload[12] != CLIENT_PLAN_CAPABILITY_OPERATION
    {
        return None;
    }
    let inner_version = u32::from_be_bytes(payload[13..17].try_into().ok()?);
    let inner_length = u32::from_be_bytes(payload[17..21].try_into().ok()?) as usize;
    let inner_end = 21usize.checked_add(inner_length)?;
    (inner_end == payload.len()).then(|| (inner_version, &payload[21..inner_end]))
}

fn run_installed_qt_external_contract(
    request: &ClientExternalContractRequest,
) -> Result<RuntimeValue, String> {
    let library =
        RuntimeLibrary::load_installed_qt().map_err(|_| "runtime.unavailable".to_owned())?;
    let session = RuntimeSession::new_qt(library, "en-GB", "UTC", "light")
        .map_err(|_| "runtime.unavailable".to_owned())?;
    let mut executor = QtRuntimeExecutor::new(session);
    let result = (|| {
        let value = ClientResourceExecutor::external_contract(&mut executor, request.clone())?;
        executor
            .wait_for_surfaces()
            .map_err(|_| "runtime.unavailable".to_owned())?;
        Ok::<RuntimeValue, String>(value)
    })();
    let shutdown = executor.shutdown();
    match result {
        Err(error) => Err(error),
        Ok(value) => {
            shutdown.map_err(|_| "runtime.unavailable".to_owned())?;
            Ok(value)
        }
    }
}

/// Evaluates the installed standard Inspector render contract without selecting a
/// graphical runtime or reading mutable state. The carrier envelope does not
/// encode its owning principal, so the full epoch is authenticated through the
/// installed session before the UI value is constructed.
pub(super) async fn run_installed_external_contract(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    active: &ActiveDatabaseRevision,
    request: &ClientExternalContractRequest,
    current_invocation: Option<InvocationId>,
) -> Result<RuntimeValue, String> {
    if request.identity() == STD_UI_WINDOW_RUNTIME_CONTRACT {
        return run_installed_qt_external_contract(request);
    }
    if request.identity() != INSPECT_RENDER_CONTRACT {
        return Err("inspect.runtime_unavailable".to_owned());
    }
    require_current_observer_invocation(current_invocation, request.observer_root_invocation_id())?;
    if request.context().pair() != active.pair() {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    let Some(definition) = active
        .catalogue()
        .function_by_id(request.context().function())
    else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let Some(revision) = active.function_revisions().iter().find(|revision| {
        revision.function() == request.context().function()
            && revision.id() == request.context().function_revision()
    }) else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if request.context().function_revision() != definition.current_revision()
        || definition.domain() != FunctionDomain::Client
        || !matches!(
            definition.return_type(),
            FunctionReturn::Single(ResolvedType::Value(type_id)) if *type_id == STD_UI_TYPE_ID
        )
        || !inspect_render_artifact_is_external(revision)
    {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let arguments = request.arguments();
    if arguments.len() != INSPECT_RENDER_CARRIER_SIGNATURE.len()
        || definition.parameters().len() != INSPECT_RENDER_CARRIER_SIGNATURE.len()
    {
        return Err("inspect.malformed_carrier".to_owned());
    }

    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| "inspect.runtime_unavailable".to_owned())?;
    let registry =
        registered_opaque_codecs(standard).map_err(|_| "inspect.runtime_unavailable".to_owned())?;
    let mut epoch_id = None;
    let mut server_epoch = None;
    let mut server_root_target = None;
    let mut target_invocation_id = None;
    let mut row_counts = Vec::with_capacity(arguments.len());
    for (index, ((parameter_id, value), (expected_name, expected_type, expected_kind))) in arguments
        .iter()
        .zip(INSPECT_RENDER_CARRIER_SIGNATURE)
        .enumerate()
    {
        let parameter = &definition.parameters()[index];
        if parameter.id() != *parameter_id
            || parameter.name() != expected_name
            || parameter.resolved_type() != ResolvedType::Value(expected_type)
        {
            return Err("inspect.malformed_carrier".to_owned());
        }
        let RuntimeValue::Opaque(value) = value else {
            return Err("inspect.malformed_carrier".to_owned());
        };
        if value.opaque_type() != expected_type {
            return Err("inspect.unknown_carrier".to_owned());
        }
        let envelope = InspectCarrierEnvelope::decode(value.canonical_payload())
            .map_err(map_inspect_carrier_error)?;
        let _validated =
            OpaqueValue::new_inspect_carrier(active, expected_type, value.canonical_payload())
                .map_err(map_inspect_opaque_value_error)?;
        if envelope.carrier_kind() != expected_kind {
            return Err("inspect.malformed_carrier".to_owned());
        }
        if envelope.source_revision_id() != active.pair().source()
            || envelope.catalogue_revision_id() != active.pair().catalogue()
        {
            return Err("inspect.epoch_mismatch".to_owned());
        }
        let mut carrier_target = None;
        if expected_kind == InspectCarrierKind::Snapshot {
            if envelope.rows().len() != 1 {
                return Err("inspect.malformed_carrier".to_owned());
            }
            let snapshot_row = envelope
                .rows()
                .first()
                .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
            let (carrier_epoch, carrier_target_id, carrier_root_target) =
                decode_snapshot_row_epoch(active, &registry, snapshot_row, envelope.epoch_id())?;
            if server_epoch.is_some_and(|known| known != carrier_epoch) {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            server_epoch = Some(carrier_epoch);
            if server_root_target.is_some_and(|known| known != carrier_root_target) {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            server_root_target = Some(carrier_root_target);
            carrier_target = Some(carrier_target_id);
        } else {
            for row in envelope.rows() {
                let (carrier_epoch, target, root_target) = decode_enriched_inspect_row_target(
                    active,
                    &registry,
                    row,
                    expected_kind,
                    envelope.epoch_id(),
                )?;
                if server_epoch.is_some_and(|known| known != carrier_epoch) {
                    return Err("inspect.epoch_mismatch".to_owned());
                }
                server_epoch = Some(carrier_epoch);
                if server_root_target.is_some_and(|known| known != root_target) {
                    return Err("inspect.epoch_mismatch".to_owned());
                }
                server_root_target = Some(root_target);
                if carrier_target.is_some_and(|known| known != target) {
                    return Err("inspect.epoch_mismatch".to_owned());
                }
                carrier_target = Some(target);
            }
        }
        if let Some(carrier_target) = carrier_target {
            if target_invocation_id.is_some_and(|known| known != carrier_target) {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            target_invocation_id = Some(carrier_target);
        }
        match epoch_id {
            Some(expected_epoch) if expected_epoch != envelope.epoch_id() => {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            None => epoch_id = Some(envelope.epoch_id()),
            _ => {}
        }
        row_counts.push(envelope.rows().len());
    }

    let epoch_id = epoch_id.ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    // The required snapshot carrier is the epoch-bearing anchor. Empty
    // projections have no row payload, so their header epoch is checked
    // against this anchor and the authenticated snapshot below.
    let server_epoch = server_epoch.ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    if server_epoch != epoch_id {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    let target_invocation_id =
        target_invocation_id.ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    let root_target = server_root_target.ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    let observer_lineage = [
        request.observer_root_invocation_id(),
        request.observer_parent_invocation_id(),
    ];
    let loaded_snapshot = authorize_inspect_target_before_recursion(
        || async {
            let Some(snapshot) = kernel
                .load_inspect_snapshot(session, server_epoch)
                .await
                .map_err(inspect_kernel_error_code)?
            else {
                return Err(INSPECT_DENIED_CODE.to_owned());
            };
            validate_epoch(&snapshot, target_invocation_id, active.pair())?;
            require_inspect_root_provenance(snapshot.root_target(), root_target)?;
            Ok(snapshot)
        },
        target_invocation_id,
        &observer_lineage,
        |observer, target| async move {
            kernel
                .inspect_target_is_recursive(observer, target)
                .await
                .map_err(inspect_kernel_error_code)
        },
    )
    .await?;
    require_inspect_observer_context(
        loaded_snapshot.observer_context(),
        request.observer_root_invocation_id(),
        request.context().parent_invocation_id(),
    )?;
    let client_epoch_id = request.context().client_epoch_id().invocation_id();
    let body = serde_json::to_vec(&serde_json::json!({
        "kind": "node",
        "contract": {
            "id": "std.ui.window",
            "name": "std.ui.window",
            "version": "1.0",
        },
        "call_site_id": null,
        "function_instance_id": null,
        "key": {
            "type": "std.types.text",
            "value": format!("inspector-{client_epoch_id}-{epoch_id}"),
        },
        "properties": {
            "client_epoch": {
                "type": "std.types.text",
                "value": client_epoch_id.to_string(),
            },
            "server_epoch": {
                "type": "std.types.text",
                "value": epoch_id.to_string(),
            },
            "carrier_rows": {
                "type": "std.types.text",
                "value": row_counts
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            },
        },
        "slots": {},
        "actions": {},
    }))
    .map_err(|_| "inspect.projection_failed".to_owned())?;
    let body_length = u32::try_from(body.len()).map_err(|_| "inspect.limit".to_owned())?;
    let mut payload = b"ORNA-UI/1 ".to_vec();
    payload.extend_from_slice(&body_length.to_be_bytes());
    payload.extend_from_slice(&body);
    OpaqueValue::new(active, &registry, STD_UI_TYPE_ID, payload)
        .map(RuntimeValue::Opaque)
        .map_err(|_| "inspect.projection_failed".to_owned())
}

#[cfg(test)]
pub(super) async fn reject_recursive_inspect_target<F, Fut>(
    target: InvocationId,
    observer_root: InvocationId,
    observer_parent: InvocationId,
    mut is_recursive: F,
) -> Result<(), String>
where
    F: FnMut(InvocationId, InvocationId) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    if is_recursive(observer_root, target).await? {
        return Err("inspect.recursion".to_owned());
    }
    if observer_parent != observer_root && is_recursive(observer_parent, target).await? {
        return Err("inspect.recursion".to_owned());
    }
    Ok(())
}

async fn reject_recursive_inspect_lineage_target<F, Fut>(
    target: InvocationId,
    observer_lineage: &[InvocationId],
    mut is_recursive: F,
) -> Result<(), String>
where
    F: FnMut(InvocationId, InvocationId) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    for observer in observer_lineage {
        if is_recursive(*observer, target).await? {
            return Err("inspect.recursion".to_owned());
        }
    }
    Ok(())
}

pub(super) async fn authorize_inspect_target_before_recursion<T, A, AFut, F, Fut>(
    authorize: A,
    target: InvocationId,
    observer_lineage: &[InvocationId],
    is_recursive: F,
) -> Result<T, String>
where
    A: FnOnce() -> AFut,
    AFut: Future<Output = Result<T, String>>,
    F: FnMut(InvocationId, InvocationId) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    let authorized = authorize().await?;
    reject_recursive_inspect_lineage_target(target, observer_lineage, is_recursive).await?;
    Ok(authorized)
}

pub(super) fn map_inspect_carrier_error(error: InspectCarrierError) -> String {
    match error {
        InspectCarrierError::EnvelopeTooLarge { .. }
        | InspectCarrierError::RowCountExceeded { .. }
        | InspectCarrierError::RowTooLarge { .. }
        | InspectCarrierError::InvalidRow(
            orna_core::inspect_carrier::InspectRowError::PayloadTooLarge { .. },
        ) => "inspect.limit".to_owned(),
        InspectCarrierError::InvalidTargetInvocation => "inspect.invalid_target".to_owned(),
        InspectCarrierError::TargetInvocationMismatch { .. } => "inspect.epoch_mismatch".to_owned(),
        _ => "inspect.malformed_carrier".to_owned(),
    }
}

pub(super) fn map_inspect_opaque_value_error(error: OpaqueValueError) -> String {
    match error {
        OpaqueValueError::UnregisteredType { .. } => "inspect.unknown_carrier".to_owned(),
        OpaqueValueError::InspectCarrierRevisionMismatch { .. } => {
            "inspect.epoch_mismatch".to_owned()
        }
        _ => "inspect.malformed_carrier".to_owned(),
    }
}

pub(super) fn require_current_observer_invocation(
    current_invocation: Option<InvocationId>,
    observer_root: InvocationId,
) -> Result<InvocationId, String> {
    let Some(current_invocation) = current_invocation else {
        return Err("inspect.epoch_mismatch".to_owned());
    };
    if observer_root != current_invocation {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(current_invocation)
}

pub(super) async fn run_installed_inspect(
    kernel: PostgresKernel,
    session: AuthenticatedSession,
    active: ActiveDatabaseRevision,
    request: ClientInspectRequest,
    current_invocation: Option<InvocationId>,
) -> Result<RuntimeValue, String> {
    let current_invocation = require_current_observer_invocation(
        current_invocation,
        request.observer_root_invocation_id(),
    )?;
    let observer_root = request.observer_root_invocation_id();
    // The enclosing server invocation is stable across the nested CLIENT
    // helper calls that make up one Inspector operation.
    let observer_parent = request.context().parent_invocation_id();

    validate_inspect_request_context(&request, &active)?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| "inspect.runtime_unavailable".to_owned())?;
    let registry =
        registered_opaque_codecs(standard).map_err(|_| "inspect.runtime_unavailable".to_owned())?;
    match request.operation() {
        ClientInspectOperation::Snapshot { target } => {
            // The installed v1 provider has no closed decoder for the opaque
            // snapshot-options carrier. Omitted options are the structural
            // default; reject supplied options rather than silently discarding
            // classifier bits.
            if request.snapshot_options().is_some() {
                return Err("inspect.invalid_options".to_owned());
            }
            let invocation = inspect_snapshot_request_target(target)?;
            require_inspect_target_provenance(request.target_invocation_id(), invocation)?;
            let authorization_kernel = kernel.clone();
            let authorization_session = session.clone();
            let authorization_pair = active.pair();
            let recursion_kernel = kernel.clone();
            let loaded_snapshot = authorize_inspect_target_before_recursion(
                move || async move {
                    let Some(epoch_id) = authorization_kernel
                        .find_latest_inspect_epoch(&authorization_session, invocation)
                        .await
                        .map_err(inspect_kernel_error_code)?
                    else {
                        return Err(INSPECT_DENIED_CODE.to_owned());
                    };
                    let Some(loaded_snapshot) = authorization_kernel
                        .load_inspect_snapshot(&authorization_session, epoch_id)
                        .await
                        .map_err(inspect_kernel_error_code)?
                    else {
                        return Err(INSPECT_DENIED_CODE.to_owned());
                    };
                    validate_epoch(&loaded_snapshot, invocation, authorization_pair)?;
                    Ok(loaded_snapshot)
                },
                invocation,
                request.observer_lineage(),
                move |observer, target| {
                    let kernel = recursion_kernel.clone();
                    async move {
                        kernel
                            .inspect_target_is_recursive(observer, target)
                            .await
                            .map_err(inspect_kernel_error_code)
                    }
                },
            )
            .await?;
            let observer_context = InspectObserverContext::new(observer_root, observer_parent)
                .map_err(|_| "inspect.epoch_mismatch".to_owned())?;
            let Some(loaded_snapshot) = kernel
                .clone_inspect_snapshot_for_current_invocation(
                    &session,
                    loaded_snapshot.id(),
                    observer_context,
                    current_invocation,
                )
                .await
                .map_err(inspect_kernel_error_code)?
            else {
                return Err(INSPECT_DENIED_CODE.to_owned());
            };
            let payload = make_inspect_carrier(
                &active,
                &registry,
                InspectCarrierKind::Snapshot,
                &loaded_snapshot,
                invocation,
                vec![encode_snapshot_row(&loaded_snapshot)],
                0,
            )?;
            make_opaque(&active, SYS_INSPECT_SNAPSHOT_TYPE_ID, payload)
        }
        ClientInspectOperation::Projection { snapshot, .. } => {
            let tag = request
                .operation()
                .projection_carrier_tag()
                .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
            let snapshot = match snapshot {
                RuntimeValue::Opaque(value)
                    if value.opaque_type() == SYS_INSPECT_SNAPSHOT_TYPE_ID =>
                {
                    value
                }
                RuntimeValue::Opaque(_) => return Err("inspect.unknown_carrier".to_owned()),
                _ => return Err("inspect.malformed_carrier".to_owned()),
            };
            let envelope = InspectCarrierEnvelope::decode(snapshot.canonical_payload())
                .map_err(map_inspect_carrier_error)?;
            if envelope.carrier_kind() != InspectCarrierKind::Snapshot {
                return Err("inspect.malformed_carrier".to_owned());
            }
            if envelope.source_revision_id() != active.pair().source()
                || envelope.catalogue_revision_id() != active.pair().catalogue()
            {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            if envelope.rows().len() != 1 {
                return Err("inspect.malformed_carrier".to_owned());
            }
            let snapshot_row = envelope
                .rows()
                .first()
                .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
            let (epoch_id, target_invocation, root_target) =
                decode_snapshot_row_epoch(&active, &registry, snapshot_row, envelope.epoch_id())?;
            validate_inspect_projection_binding(
                request.target_invocation_id(),
                &envelope,
                epoch_id,
                target_invocation,
                active.pair(),
            )?;
            let authorization_kernel = kernel.clone();
            let authorization_session = session.clone();
            let authorization_pair = active.pair();
            let recursion_kernel = kernel.clone();
            let loaded_snapshot = authorize_inspect_target_before_recursion(
                move || async move {
                    let Some(_) = authorization_kernel
                        .find_inspect_epoch(&authorization_session, epoch_id)
                        .await
                        .map_err(inspect_kernel_error_code)?
                    else {
                        return Err(INSPECT_DENIED_CODE.to_owned());
                    };
                    let Some(loaded_snapshot) = authorization_kernel
                        .load_inspect_snapshot(&authorization_session, epoch_id)
                        .await
                        .map_err(inspect_kernel_error_code)?
                    else {
                        return Err(INSPECT_DENIED_CODE.to_owned());
                    };
                    validate_epoch(&loaded_snapshot, target_invocation, authorization_pair)?;
                    require_inspect_root_provenance(loaded_snapshot.root_target(), root_target)?;
                    require_inspect_observer_context(
                        loaded_snapshot.observer_context(),
                        observer_root,
                        observer_parent,
                    )?;
                    Ok(loaded_snapshot)
                },
                target_invocation,
                request.observer_lineage(),
                move |observer, target| {
                    let kernel = recursion_kernel.clone();
                    async move {
                        kernel
                            .inspect_target_is_recursive(observer, target)
                            .await
                            .map_err(inspect_kernel_error_code)
                    }
                },
            )
            .await?;
            let privilege = InspectPrivilege::OwnInvocation;
            let granted = [InspectPrivilege::OwnInvocation];
            let values_granted = inspect_classifier_granted(&granted, InspectPrivilege::Values);
            let security_details_granted =
                inspect_classifier_granted(&granted, InspectPrivilege::SecurityDetails);
            let runtime_internals_granted =
                inspect_classifier_granted(&granted, InspectPrivilege::RuntimeInternals);
            let source_granted = inspect_classifier_granted(&granted, InspectPrivilege::Source);
            // The installed v1 request carries only the structural
            // OwnInvocation grant. The epoch rows are immutable, but the
            // carrier boundary still enforces each classifier independently:
            // a future armed path may pass the matching protected grant, while
            // this ordinary path emits typed redaction markers.
            let rows = match tag {
                2 => encode_invocation_nodes(
                    &kernel
                        .inspect_invocation_nodes(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                ),
                3 => encode_calls(
                    &kernel
                        .inspect_calls(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    values_granted,
                ),
                4 => encode_resources(
                    &kernel
                        .inspect_resources(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                ),
                5 => encode_state_cells(
                    &kernel
                        .inspect_state_cells(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                ),
                6 => encode_ui_nodes(
                    &kernel
                        .inspect_ui_nodes(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    source_granted,
                    runtime_internals_granted,
                ),
                7 => encode_presentation_candidates(
                    &kernel
                        .inspect_presentation_candidates(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    runtime_internals_granted,
                ),
                8 => encode_runtime_bindings(
                    &kernel
                        .inspect_runtime_bindings(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    runtime_internals_granted,
                ),
                9 => encode_security_decisions(
                    &kernel
                        .inspect_security_decisions(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    security_details_granted,
                ),
                _ => return Err("inspect.malformed_carrier".to_owned()),
            }?;
            let kind = InspectCarrierKind::from_tag(tag)
                .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
            let payload = make_inspect_carrier(
                &active,
                &registry,
                kind,
                &loaded_snapshot,
                target_invocation,
                rows,
                inspect_classification_tag(kind, privilege),
            )?;
            make_opaque(&active, kind.type_id(), payload)
        }
    }
}

pub(super) fn inspect_snapshot_request_target(
    target: &RuntimeValue,
) -> Result<InvocationId, String> {
    let RuntimeValue::Reference { target, object } = target else {
        return Err("inspect.invalid_target".to_owned());
    };
    let object_bytes = object.to_bytes();
    if *target != SYS_INSPECT_INVOCATION_TYPE_ID || object_bytes == [0; 16] {
        return Err("inspect.invalid_target".to_owned());
    }
    Ok(InvocationId::from_bytes(object_bytes))
}

fn validate_inspect_request_context(
    request: &ClientInspectRequest,
    active: &ActiveDatabaseRevision,
) -> Result<(), String> {
    if request.context().pair() != active.pair() {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

pub(super) fn require_inspect_target_provenance(
    request_target: Option<InvocationId>,
    decoded_target: InvocationId,
) -> Result<(), String> {
    let Some(request_target) = request_target else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if request_target != decoded_target {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

pub(super) fn require_inspect_root_provenance(
    snapshot_root: FunctionId,
    decoded_root: FunctionId,
) -> Result<(), String> {
    if snapshot_root != decoded_root {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

pub(super) fn require_inspect_observer_context(
    stored: Option<InspectObserverContext>,
    observer_root: InvocationId,
    observer_parent: InvocationId,
) -> Result<(), String> {
    let expected = InspectObserverContext::new(observer_root, observer_parent)
        .map_err(|_| "inspect.epoch_mismatch".to_owned())?;
    if stored != Some(expected) {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

pub(super) fn validate_inspect_projection_binding(
    request_target: Option<InvocationId>,
    envelope: &InspectCarrierEnvelope,
    decoded_epoch: InspectEpochId,
    decoded_target: InvocationId,
    pair: orna_core::revision::RevisionPair,
) -> Result<(), String> {
    require_inspect_target_provenance(request_target, decoded_target)?;
    if envelope
        .target_invocation_id()
        .is_some_and(|target| target != decoded_target)
    {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    if envelope.epoch_id() != decoded_epoch {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    if envelope.source_revision_id() != pair.source()
        || envelope.catalogue_revision_id() != pair.catalogue()
    {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

pub(super) fn inspect_kernel_error_code(error: PostgresKernelError) -> String {
    match error {
        PostgresKernelError::InspectDenied { reason } => match reason {
            orna_core::security::InspectDenial::MissingEpoch
            | orna_core::security::InspectDenial::MissingPrivilege => {
                INSPECT_DENIED_CODE.to_owned()
            }
            orna_core::security::InspectDenial::ObserverSuppressed => {
                "inspect.recursion".to_owned()
            }
        },
        _ => "inspect.projection_failed".to_owned(),
    }
}

fn validate_epoch(
    snapshot: &AuthenticatedInspectSnapshot,
    invocation: InvocationId,
    pair: orna_core::revision::RevisionPair,
) -> Result<(), String> {
    if snapshot.invocation_id() != invocation {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    if snapshot.source_revision_id() != pair.source()
        || snapshot.catalogue_revision_id() != pair.catalogue()
    {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

fn make_opaque(
    active: &ActiveDatabaseRevision,
    opaque_type: TypeId,
    payload: Vec<u8>,
) -> Result<RuntimeValue, String> {
    OpaqueValue::new_inspect_carrier(active, opaque_type, payload)
        .map(RuntimeValue::Opaque)
        .map_err(|_| "inspect.projection_failed".to_owned())
}
pub(super) trait InspectCarrierSnapshot {
    fn id(&self) -> InspectEpochId;
    fn invocation_id(&self) -> InvocationId;
    fn root_target(&self) -> FunctionId;
    fn source_revision_id(&self) -> SourceRevisionId;
    fn catalogue_revision_id(&self) -> CatalogueRevisionId;
}

impl InspectCarrierSnapshot for AuthenticatedInspectSnapshot {
    fn id(&self) -> InspectEpochId {
        AuthenticatedInspectSnapshot::id(self)
    }

    fn invocation_id(&self) -> InvocationId {
        AuthenticatedInspectSnapshot::invocation_id(self)
    }

    fn root_target(&self) -> FunctionId {
        AuthenticatedInspectSnapshot::root_target(self)
    }

    fn source_revision_id(&self) -> SourceRevisionId {
        AuthenticatedInspectSnapshot::source_revision_id(self)
    }

    fn catalogue_revision_id(&self) -> CatalogueRevisionId {
        AuthenticatedInspectSnapshot::catalogue_revision_id(self)
    }
}

#[cfg(test)]
impl InspectCarrierSnapshot for orna_core::inspect::InspectSnapshotEpoch {
    fn id(&self) -> InspectEpochId {
        self.id()
    }

    fn invocation_id(&self) -> InvocationId {
        self.invocation_id()
    }

    fn root_target(&self) -> FunctionId {
        self.root_target()
    }

    fn source_revision_id(&self) -> SourceRevisionId {
        self.source_revision_id()
    }

    fn catalogue_revision_id(&self) -> CatalogueRevisionId {
        self.catalogue_revision_id()
    }
}

pub(super) fn make_inspect_carrier(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    kind: InspectCarrierKind,
    snapshot: &impl InspectCarrierSnapshot,
    target_invocation: InvocationId,
    rows: Vec<Vec<u8>>,
    classification: u8,
) -> Result<Vec<u8>, String> {
    let epoch_id = snapshot.id();
    let mut encoded_rows = rows
        .into_iter()
        .map(|row| {
            let row = if kind == InspectCarrierKind::Snapshot {
                row
            } else {
                enrich_inspect_row(snapshot, row, classification)
            };
            encode_inspect_row(active, registry, row)
        })
        .collect::<Result<Vec<_>, _>>()?;
    encoded_rows.sort_unstable();
    InspectCarrierEnvelope::new_with_target(
        kind,
        target_invocation,
        InspectCarrierProvenance::trusted(
            epoch_id,
            snapshot.source_revision_id(),
            snapshot.catalogue_revision_id(),
        ),
        encoded_rows,
    )
    .and_then(|envelope| envelope.encode())
    .map_err(|_| "inspect.projection_failed".to_owned())
}

/// Wraps one accepted Inspector identity payload in the canonical ORV5
/// constructed-value codec. The list descriptor and byte child are fixed so
/// every projection has one deterministic row representation while the
/// existing row payload remains intact inside the ORV5 value.
pub(super) fn encode_inspect_row(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    row: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let descriptor = TypeDescriptor::list(TypeDescriptor::named(BINARY_LARGE_OBJECT_TYPE_ID))
        .map_err(|_| "inspect.projection_failed".to_owned())?;
    let value = RuntimeValue::list(active, descriptor, vec![RuntimeValue::Bytes(row)])
        .map_err(|_| "inspect.projection_failed".to_owned())?;
    encode_constructed_value(active, registry, &value)
        .map_err(|_| "inspect.projection_failed".to_owned())
}

pub(super) fn row(tag: u8, index: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(128);
    bytes.push(tag);
    bytes.extend_from_slice(&(index as u64).to_be_bytes());
    bytes
}

/// Adds the canonical common row evidence required by ADR 0080. The
/// projection-specific encoders retain their complete row fields after this
/// fixed header; all identities use their full sixteen-byte form.
fn enrich_inspect_row(
    snapshot: &impl InspectCarrierSnapshot,
    row: Vec<u8>,
    classification: u8,
) -> Vec<u8> {
    if row.len() < 9 {
        return row;
    }
    let mut enriched = Vec::with_capacity(row.len() + 82);
    enriched.extend_from_slice(&row[..9]);
    // Fixed common prefix: epoch identity, target invocation, root target,
    // pinned revisions, own-invocation scope, and classifier evidence. The
    // owner principal is deliberately not copied: the scope fact is enough
    // for this CLIENT carrier and the principal is security-classified.
    enriched.extend_from_slice(&snapshot.id().to_bytes());
    enriched.extend_from_slice(&snapshot.invocation_id().to_bytes());
    enriched.extend_from_slice(&snapshot.root_target().to_bytes());
    enriched.extend_from_slice(&snapshot.source_revision_id().to_bytes());
    enriched.extend_from_slice(&snapshot.catalogue_revision_id().to_bytes());
    enriched.push(1);
    enriched.push(classification);
    enriched.extend_from_slice(&row[9..]);
    enriched
}

pub(super) fn inspect_classification_tag(
    kind: InspectCarrierKind,
    privilege: InspectPrivilege,
) -> u8 {
    match (kind, privilege) {
        (InspectCarrierKind::StateCells, InspectPrivilege::Values) => 1,
        (InspectCarrierKind::SecurityDecisions, InspectPrivilege::SecurityDetails) => 3,
        (InspectCarrierKind::RuntimeBindings, InspectPrivilege::RuntimeInternals) => 4,
        _ => 0,
    }
}

pub(super) fn inspect_classifier_granted(
    granted: &[InspectPrivilege],
    classifier: InspectPrivilege,
) -> bool {
    debug_assert!(classifier.is_classifier());
    granted.contains(&classifier)
}

fn encode_classified_text(bytes: &mut Vec<u8>, value: &str, granted: bool) -> Result<(), String> {
    if !granted {
        bytes.extend_from_slice(&INSPECT_REDACTED_TEXT_LENGTH.to_be_bytes());
        return Ok(());
    }
    text(bytes, value)
}

fn encode_classified_optional_text(
    bytes: &mut Vec<u8>,
    value: Option<&str>,
    granted: bool,
) -> Result<(), String> {
    if !granted {
        bytes.push(INSPECT_REDACTED_FIELD_TAG);
        return Ok(());
    }
    match value {
        Some(value) => {
            bytes.push(1);
            text(bytes, value)?;
        }
        None => bytes.push(0),
    }
    Ok(())
}

/// Encodes an optional descriptor with the persisted projection convention.
///
/// A denied classified field gets the same marker used by the other optional
/// classified fields, with no presence bit or descriptor bytes. When the
/// classifier is granted, the field uses the persisted TypeDescriptor tags so
/// the selected sink remains a complete canonical descriptor.
fn encode_classified_optional_descriptor(
    bytes: &mut Vec<u8>,
    value: Option<&TypeDescriptor>,
    granted: bool,
) -> Result<(), String> {
    if !granted {
        bytes.push(INSPECT_REDACTED_FIELD_TAG);
        return Ok(());
    }
    match value {
        Some(value) => {
            bytes.push(1);
            encode_type_descriptor(bytes, value)?;
        }
        None => bytes.push(0),
    }
    Ok(())
}

/// Encodes a TypeDescriptor using the canonical persisted projection tags.
fn encode_type_descriptor(bytes: &mut Vec<u8>, descriptor: &TypeDescriptor) -> Result<(), String> {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) => {
            bytes.push(0);
            id(bytes, &type_id.to_bytes());
        }
        TypeDescriptorKind::Reference(type_id) => {
            bytes.push(1);
            id(bytes, &type_id.to_bytes());
        }
        TypeDescriptorKind::List(element) => {
            bytes.push(2);
            encode_type_descriptor(bytes, element)?;
        }
        TypeDescriptorKind::Set(element) => {
            bytes.push(3);
            encode_type_descriptor(bytes, element)?;
        }
        TypeDescriptorKind::Map { key, value } => {
            bytes.push(4);
            encode_type_descriptor(bytes, key)?;
            encode_type_descriptor(bytes, value)?;
        }
        TypeDescriptorKind::Option(value) => {
            bytes.push(5);
            encode_type_descriptor(bytes, value)?;
        }
        TypeDescriptorKind::Stream(element) => {
            bytes.push(6);
            encode_type_descriptor(bytes, element)?;
        }
    }
    Ok(())
}

pub(super) fn id(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(value);
}
fn text(bytes: &mut Vec<u8>, value: &str) -> Result<(), String> {
    if value.len() > 65_536 {
        return Err("inspect.projection_failed".to_owned());
    }
    bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn encode_snapshot_row(snapshot: &AuthenticatedInspectSnapshot) -> Vec<u8> {
    let mut bytes = row(INSPECT_SNAPSHOT_ROW_TAG, 0);
    id(&mut bytes, &snapshot.id().to_bytes());
    id(&mut bytes, &snapshot.invocation_id().to_bytes());
    id(&mut bytes, &snapshot.root_target().to_bytes());
    bytes.push(match snapshot.outcome() {
        InspectOutcomeKind::Allowed => 1,
        InspectOutcomeKind::Denied => 2,
        InspectOutcomeKind::Failed => 3,
        InspectOutcomeKind::Cancelled => 4,
    });
    let summary = snapshot.summary();
    bytes.extend_from_slice(&summary.event_count().to_be_bytes());
    match summary.result() {
        InspectResultSummary::NoValues => bytes.push(0),
        InspectResultSummary::ValueBatch { value_count } => {
            bytes.push(1);
            bytes.extend_from_slice(&value_count.to_be_bytes());
        }
    }
    match summary.duration_nanoseconds() {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
    bytes
}
pub(super) fn decode_enriched_inspect_row_target(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    row: &[u8],
    expected_kind: InspectCarrierKind,
    epoch_id: InspectEpochId,
) -> Result<(InspectEpochId, InvocationId, FunctionId), String> {
    let value = decode_constructed_value(active, registry, row)
        .map_err(|_| "inspect.malformed_carrier".to_owned())?;
    let RuntimeValue::Constructed(constructed) = value else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let TypeDescriptorKind::List(child) = constructed.descriptor().kind() else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if child.kind() != TypeDescriptorKind::Named(BINARY_LARGE_OBJECT_TYPE_ID) {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let ConstructedValueKind::List(values) = constructed.kind() else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if payload.len() < 91 || payload[0] != expected_kind.tag() {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let epoch = InspectEpochId::from_bytes(
        payload[9..25]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    if epoch != epoch_id {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    if payload[57..73] != active.pair().source().to_bytes()
        || payload[73..89] != active.pair().catalogue().to_bytes()
    {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    if payload[89] != 1 || payload[90] > 4 {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let target = InvocationId::from_bytes(
        payload[25..41]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    if target.to_bytes() == [0; 16] {
        return Err("inspect.invalid_target".to_owned());
    }
    let root_target = FunctionId::from_bytes(
        payload[41..57]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    Ok((epoch, target, root_target))
}

fn decode_snapshot_row_epoch(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    row: &[u8],
    epoch_id: InspectEpochId,
) -> Result<(InspectEpochId, InvocationId, FunctionId), String> {
    let value = decode_constructed_value(active, registry, row)
        .map_err(|_| "inspect.malformed_carrier".to_owned())?;
    let RuntimeValue::Constructed(constructed) = value else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let TypeDescriptorKind::List(child) = constructed.descriptor().kind() else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if child.kind() != TypeDescriptorKind::Named(BINARY_LARGE_OBJECT_TYPE_ID) {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let ConstructedValueKind::List(values) = constructed.kind() else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    decode_snapshot_row_payload(payload, epoch_id)
}

pub(super) fn decode_snapshot_row_payload(
    row: &[u8],
    epoch_id: InspectEpochId,
) -> Result<(InspectEpochId, InvocationId, FunctionId), String> {
    if row.len() < 68 || row[0] != INSPECT_SNAPSHOT_ROW_TAG || row[1..9] != [0; 8] {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let bytes: [u8; 16] = row[9..25]
        .try_into()
        .map_err(|_| "inspect.malformed_carrier".to_owned())?;
    let id = InspectEpochId::from_bytes(bytes);
    if id != epoch_id {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    let invocation = InvocationId::from_bytes(
        row[25..41]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    if invocation.to_bytes() == [0; 16] {
        return Err("inspect.invalid_target".to_owned());
    }
    let root_target = FunctionId::from_bytes(
        row[41..57]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    let mut offset = 57;
    let outcome = *row
        .get(offset)
        .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    if !(1..=4).contains(&outcome) {
        return Err("inspect.malformed_carrier".to_owned());
    }
    offset += 1 + 8;
    let result = *row
        .get(offset)
        .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    offset += 1;
    if result == 1 {
        let value_count = row
            .get(offset..)
            .and_then(|bytes| bytes.get(..8))
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_be_bytes)
            .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
        if value_count == 0 {
            return Err("inspect.malformed_carrier".to_owned());
        }
        offset += 8;
    } else if result != 0 {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let duration = *row
        .get(offset)
        .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    offset += 1;
    if duration == 1 {
        offset += 8;
    } else if duration != 0 {
        return Err("inspect.malformed_carrier".to_owned());
    }
    if offset != row.len() {
        return Err("inspect.malformed_carrier".to_owned());
    }
    Ok((id, invocation, root_target))
}

fn encode_invocation_nodes(rows: &[InvocationNodeRow]) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(2, index);
            id(&mut bytes, &value.id().to_bytes());
            match value.parent_id() {
                Some(parent) => {
                    bytes.push(1);
                    id(&mut bytes, &parent.to_bytes());
                }
                None => bytes.push(0),
            };
            bytes.push(match value.kind() {
                InspectInvocationNodeKind::Root => 1,
                InspectInvocationNodeKind::Nested => 2,
            });
            bytes.push(match value.phase() {
                InspectInvocationPhase::Started => 1,
                InspectInvocationPhase::Executing => 2,
                InspectInvocationPhase::Completed => 3,
                InspectInvocationPhase::Failed => 4,
                InspectInvocationPhase::Cancelled => 5,
            });
            id(&mut bytes, &value.target().to_bytes());
            bytes.extend_from_slice(&value.sequence().to_be_bytes());
            Ok(bytes)
        })
        .collect()
}
pub(super) fn encode_calls(rows: &[CallRow], values_granted: bool) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(3, index);
            id(&mut bytes, &value.invocation_id().to_bytes());
            bytes.push(u8::from(values_granted && value.schema().is_some()));
            bytes.extend_from_slice(&value.value_count().to_be_bytes());
            bytes.extend_from_slice(&value.duration_nanoseconds().to_be_bytes());
            Ok(bytes)
        })
        .collect()
}
fn encode_resources(rows: &[ResourceRow]) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(4, index);
            bytes.push(match value.kind() {
                InspectResourceKind::State => 1,
                InspectResourceKind::Catalog => 2,
                InspectResourceKind::Standard => 3,
                InspectResourceKind::Runtime => 4,
            });
            bytes.push(match value.status() {
                InspectResourceStatus::Active => 1,
                InspectResourceStatus::Invalidated => 2,
                InspectResourceStatus::Released => 3,
            });
            Ok(bytes)
        })
        .collect()
}
fn encode_state_cells(rows: &[StateCellRow]) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let key = value.key();
            let mut bytes = row(5, index);
            id(&mut bytes, &key.root_function().to_bytes());
            text(&mut bytes, key.state_profile())?;
            id(&mut bytes, &key.function().to_bytes());
            text(&mut bytes, key.instance_key())?;
            id(&mut bytes, &key.state_slot().to_bytes());
            id(&mut bytes, &value.value_type().to_bytes());
            bytes.extend_from_slice(&value.revision().to_be_bytes());
            bytes.push(u8::from(value.value().is_some()));
            Ok(bytes)
        })
        .collect()
}
pub(super) fn encode_ui_nodes(
    rows: &[UiNodeRow],
    source_granted: bool,
    runtime_internals_granted: bool,
) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(6, index);
            id(&mut bytes, &value.function().to_bytes());
            encode_classified_text(&mut bytes, value.call_site(), source_granted)?;
            encode_classified_text(
                &mut bytes,
                value.runtime_contract(),
                runtime_internals_granted,
            )?;
            Ok(bytes)
        })
        .collect()
}
pub(super) fn encode_presentation_candidates(
    rows: &[PresentationCandidateRow],
    runtime_internals_granted: bool,
) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(7, index);
            encode_classified_text(&mut bytes, value.presenter(), runtime_internals_granted)?;
            bytes.push(u8::from(value.accepted()));
            encode_classified_text(&mut bytes, value.reason(), runtime_internals_granted)?;
            encode_classified_optional_descriptor(
                &mut bytes,
                value.selected_sink(),
                runtime_internals_granted,
            )?;
            encode_classified_optional_text(
                &mut bytes,
                value.runtime(),
                runtime_internals_granted,
            )?;
            Ok(bytes)
        })
        .collect()
}
pub(super) fn encode_runtime_bindings(
    rows: &[RuntimeBindingRow],
    runtime_internals_granted: bool,
) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(8, index);
            if !runtime_internals_granted {
                bytes.push(INSPECT_REDACTED_FIELD_TAG);
                return Ok(bytes);
            }
            text(&mut bytes, value.runtime_name())?;
            text(&mut bytes, value.version())?;
            bytes.push(u8::from(value.trusted()));
            bytes.extend_from_slice(&value.preference_rank().to_be_bytes());
            bytes.extend_from_slice(&(value.consumed_descriptors().len() as u32).to_be_bytes());
            bytes.extend_from_slice(&(value.contracts().len() as u32).to_be_bytes());
            Ok(bytes)
        })
        .collect()
}
pub(super) fn encode_security_decisions(
    rows: &[SecurityDecisionRow],
    security_details_granted: bool,
) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(9, index);
            bytes.push(match value.kind() {
                InspectSecurityDecisionKind::Execute => 1,
                InspectSecurityDecisionKind::Capability => 2,
                InspectSecurityDecisionKind::UserState => 3,
                InspectSecurityDecisionKind::Inspect => 4,
            });
            bytes.push(match value.outcome() {
                InspectSecurityDecisionOutcome::Allowed => 1,
                InspectSecurityDecisionOutcome::Denied => 2,
            });
            match value.target() {
                Some(target) => {
                    bytes.push(1);
                    text(&mut bytes, &target.canonical())?;
                }
                None => bytes.push(0),
            }
            if !security_details_granted {
                bytes.push(INSPECT_REDACTED_FIELD_TAG);
                return Ok(bytes);
            }
            match value.denial_reason() {
                Some(reason) => {
                    bytes.push(1);
                    text(&mut bytes, reason)?;
                }
                None => bytes.push(0),
            }
            bytes.extend_from_slice(&(value.principals().len() as u32).to_be_bytes());
            for principal in value.principals() {
                text(&mut bytes, &principal.canonical())?;
            }
            bytes.extend_from_slice(&(value.audit_refs().len() as u32).to_be_bytes());
            for event in value.audit_refs() {
                text(&mut bytes, &event.canonical())?;
            }
            Ok(bytes)
        })
        .collect()
}

pub(super) fn same_resource_request_identity(
    expected: &ClientResourceRequest,
    actual: &ClientResourceRequest,
) -> bool {
    expected.request_id() == actual.request_id()
        && expected.key() == actual.key()
        && expected.generation() == actual.generation()
}
