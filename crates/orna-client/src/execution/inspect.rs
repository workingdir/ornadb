use super::*;

pub(crate) fn stable_inspect_provider_error(error: &str) -> String {
    stable_inspect_error_code(error).to_owned()
}

// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub(super) fn evaluate_external_contract(
    identity: &str,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
) -> Result<RuntimeValue, ClientExecutionError> {
    let Some(executor) = executor.as_deref_mut() else {
        if identity == INSPECT_RENDER_CONTRACT {
            return Err(ClientExecutionError::Inspect {
                context,
                source: ClientInspectError::Failed("inspect.runtime_unavailable".to_owned()),
            });
        }
        return Err(ClientExecutionError::ExternalContract {
            context,
            identity: identity.to_owned(),
        });
    };
    let request =
        ClientExternalContractRequest::with_lineage(context, identity, arguments.to_vec(), lineage);
    executor.external_contract(request).map_err(|code| {
        if identity == INSPECT_RENDER_CONTRACT {
            ClientExecutionError::Inspect {
                context,
                source: ClientInspectError::Failed(
                    if code == EXTERNAL_CONTRACT_RUNTIME_UNAVAILABLE {
                        "inspect.runtime_unavailable".to_owned()
                    } else {
                        stable_inspect_provider_error(&code)
                    },
                ),
            }
        } else {
            ClientExecutionError::ExternalContract {
                context,
                identity: identity.to_owned(),
            }
        }
    })
}

fn inspect_render_contract_error(context: ClientExecutionContext) -> ClientExecutionError {
    ClientExecutionError::Inspect {
        context,
        source: inspect_carrier_error("inspect.malformed_carrier"),
    }
}

fn inspect_render_artifact_is_external(
    revision: &orna_core::revision::FunctionRevisionRecord,
) -> bool {
    fn is_external(expression: &ClientExpressionNode) -> bool {
        matches!(
            expression,
            ClientExpressionNode::ExternalContract { identity }
                if identity == INSPECT_RENDER_CONTRACT
        )
    }

    match revision.artifact().version() {
        EXPRESSION_FORMAT_VERSION => ExpressionClientPlan::decode(revision.artifact().payload())
            .ok()
            .is_some_and(|plan| is_external(plan.expression())),
        CAPABILITY_FORMAT_VERSION => CapabilityClientPlan::decode(revision.artifact().payload())
            .ok()
            .and_then(|plan| match plan.inner_plan() {
                InnerClientPlan::Expression(expression) => {
                    Some(is_external(expression.expression()))
                }
                _ => None,
            })
            .unwrap_or(false),
        _ => false,
    }
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub(super) fn validate_inspect_render_contract(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    identity: &str,
    arguments: &[(ParameterId, RuntimeValue)],
) -> Result<(), ClientExecutionError> {
    if identity != INSPECT_RENDER_CONTRACT || context.pair() != active.pair() {
        return Err(inspect_render_contract_error(context));
    }
    let Some(definition) = active.catalogue().function_by_id(context.function()) else {
        return Err(inspect_render_contract_error(context));
    };
    let Some(revision) = active.function_revisions().iter().find(|revision| {
        revision.function() == context.function() && revision.id() == context.function_revision()
    }) else {
        return Err(inspect_render_contract_error(context));
    };
    if definition.domain() != FunctionDomain::Client
        || definition.current_revision() != context.function_revision()
        || !matches!(
            definition.return_type(),
            FunctionReturn::Single(ResolvedType::Value(type_id)) if *type_id == STD_UI_TYPE_ID
        )
        || definition.parameters().len() != INSPECT_RENDER_CARRIER_SIGNATURE.len()
        || arguments.len() != INSPECT_RENDER_CARRIER_SIGNATURE.len()
        || !inspect_render_artifact_is_external(revision)
    {
        return Err(inspect_render_contract_error(context));
    }
    for (index, ((parameter_id, value), (expected_name, expected_type, _))) in arguments
        .iter()
        .zip(INSPECT_RENDER_CARRIER_SIGNATURE)
        .enumerate()
    {
        let parameter = &definition.parameters()[index];
        if parameter.id() != *parameter_id
            || parameter.name() != expected_name
            || parameter.resolved_type() != ResolvedType::Value(expected_type)
            || !runtime_value_matches(active, value, ResolvedType::Value(expected_type))
        {
            return Err(inspect_render_contract_error(context));
        }
    }
    let Some((_, snapshot)) = arguments.first() else {
        return Err(inspect_render_contract_error(context));
    };
    let snapshot = decode_inspect_carrier(active, snapshot, SYS_INSPECT_SNAPSHOT_TYPE_ID)
        .map_err(|_| inspect_render_contract_error(context))?;
    let snapshot_target = inspect_snapshot_target_from_envelope(active, &snapshot)
        .map_err(|_| inspect_render_contract_error(context))?;

    // The render provider is a generic executor boundary, so it cannot rely on
    // the installed server provider's request-side checks. Validate every
    // carrier against the decoded snapshot before allowing the provider to
    // render. ORNA-INSPECT/1 intentionally omits target provenance from the
    // envelope; projection rows retain that fact in memory when populated.
    // Empty projections remain valid, but then there is no carrier-local target
    // evidence to compare (the opaque API exposes no generic target metadata).
    for ((_, value), (_, expected_type, expected_kind)) in
        arguments.iter().zip(INSPECT_RENDER_CARRIER_SIGNATURE)
    {
        let carrier = decode_inspect_carrier(active, value, expected_type)
            .map_err(|_| inspect_render_contract_error(context))?;
        inspect_carrier_matches_snapshot(
            active,
            &snapshot,
            snapshot_target,
            expected_kind,
            &carrier,
        )
        .map_err(|_| inspect_render_contract_error(context))?;
    }
    Ok(())
}

pub(super) fn inspect_render_ui_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> bool {
    let RuntimeValue::Opaque(opaque) = value else {
        return false;
    };
    if opaque.opaque_type() != STD_UI_TYPE_ID {
        return false;
    }
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return false;
    };
    let Ok(registry) = registered_opaque_codecs(standard) else {
        return false;
    };
    OpaqueValue::new(
        active,
        &registry,
        STD_UI_TYPE_ID,
        opaque.canonical_payload(),
    )
    .is_ok()
}

fn inspect_carrier_error(code: &'static str) -> ClientInspectError {
    ClientInspectError::Failed(code.to_owned())
}

pub(super) fn decode_inspect_carrier_payload(
    active: &ActiveDatabaseRevision,
    payload: &[u8],
    expected: TypeId,
) -> Result<InspectCarrierEnvelope, ClientInspectError> {
    let Some(kind) = InspectCarrierKind::from_type_id(expected) else {
        return Err(inspect_carrier_error("inspect.unknown_carrier"));
    };
    let envelope = InspectCarrierEnvelope::decode(payload)
        .map_err(|_| inspect_carrier_error("inspect.malformed_carrier"))?;
    if envelope.carrier_kind() != kind {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    let pair = active.pair();
    if envelope.source_revision_id() != pair.source()
        || envelope.catalogue_revision_id() != pair.catalogue()
    {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    Ok(envelope)
}

fn decode_inspect_carrier(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: TypeId,
) -> Result<InspectCarrierEnvelope, ClientInspectError> {
    let RuntimeValue::Opaque(opaque) = value else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    if opaque.opaque_type() != expected {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    decode_inspect_carrier_payload(active, opaque.canonical_payload(), expected)
}

/// Decodes one canonical ORV5 row into the opaque byte payload emitted by the
/// installed Inspector provider.
///
/// Projection carrier provenance is carried in this in-memory row prefix, not
/// in the ORNA-INSPECT/1 envelope. Keep this decoder local to the client: the
/// opaque carrier API intentionally exposes no generic row/provenance object.
fn decode_inspect_carrier_row_payload(
    active: &ActiveDatabaseRevision,
    row: &[u8],
) -> Result<Vec<u8>, ClientInspectError> {
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Err(inspect_carrier_error("inspect.projection_failed"));
    };
    let registry = registered_opaque_codecs(standard)
        .map_err(|_| inspect_carrier_error("inspect.projection_failed"))?;
    let row = decode_constructed_value(active, &registry, row)
        .map_err(|_| inspect_carrier_error("inspect.malformed_carrier"))?;
    let RuntimeValue::Constructed(constructed) = row else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    let TypeDescriptorKind::List(child) = constructed.descriptor().kind() else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    if child.kind() != TypeDescriptorKind::Named(BINARY_LARGE_OBJECT_TYPE_ID) {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    let ConstructedValueKind::List(values) = constructed.kind() else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    Ok(payload.clone())
}

/// Returns the target invocation proven by projection rows, if any.
///
/// A projection with no rows is valid (notably the currently accepted
/// resource/UI carriers), so it returns None rather than treating an empty
/// payload as malformed. A non-empty row must carry the common provenance
/// prefix emitted by the installed provider; accepting an unrecognised row
/// would let a custom provider bypass target/revision binding.
fn inspect_projection_target_from_envelope(
    active: &ActiveDatabaseRevision,
    envelope: &InspectCarrierEnvelope,
    expected_kind: InspectCarrierKind,
) -> Result<Option<InvocationId>, ClientInspectError> {
    let mut target = None;
    for row in envelope.rows() {
        let payload = decode_inspect_carrier_row_payload(active, row)?;
        if payload.len() < 91 || payload[0] != expected_kind.tag() {
            return Err(inspect_carrier_error("inspect.malformed_carrier"));
        }
        if InspectEpochId::from_bytes(payload[9..25].try_into().expect("projection epoch width"))
            != envelope.epoch_id()
        {
            return Err(inspect_carrier_error("inspect.epoch_mismatch"));
        }
        if payload[57..73] != active.pair().source().to_bytes()
            || payload[73..89] != active.pair().catalogue().to_bytes()
        {
            return Err(inspect_carrier_error("inspect.epoch_mismatch"));
        }
        if payload[89] != 1 || payload[90] > 4 {
            return Err(inspect_carrier_error("inspect.malformed_carrier"));
        }
        let row_target =
            InvocationId::from_bytes(payload[25..41].try_into().expect("projection target width"));
        if row_target.to_bytes() == [0; 16] {
            return Err(inspect_carrier_error("inspect.invalid_target"));
        }
        if target.is_some_and(|known| known != row_target) {
            return Err(inspect_carrier_error("inspect.epoch_mismatch"));
        }
        target = Some(row_target);
    }
    Ok(target)
}

/// Checks one carrier's accepted provenance against the render snapshot.
fn inspect_carrier_matches_snapshot(
    active: &ActiveDatabaseRevision,
    snapshot: &InspectCarrierEnvelope,
    snapshot_target: InvocationId,
    expected_kind: InspectCarrierKind,
    carrier: &InspectCarrierEnvelope,
) -> Result<(), ClientInspectError> {
    if carrier.carrier_kind() != expected_kind {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    if carrier.source_revision_id() != snapshot.source_revision_id()
        || carrier.catalogue_revision_id() != snapshot.catalogue_revision_id()
    {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    if carrier.epoch_id() != snapshot.epoch_id() {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    if expected_kind == InspectCarrierKind::Snapshot {
        let target = inspect_snapshot_target_from_envelope(active, carrier)?;
        if target != snapshot_target {
            return Err(inspect_carrier_error("inspect.epoch_mismatch"));
        }
        return Ok(());
    }
    if let Some(target) = inspect_projection_target_from_envelope(active, carrier, expected_kind)?
        && target != snapshot_target
    {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    Ok(())
}

fn inspect_projection_result_type(projection: InspectProjection) -> TypeId {
    match projection {
        InspectProjection::InvocationNodes => SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
        InspectProjection::Calls => SYS_INSPECT_CALLS_TYPE_ID,
        InspectProjection::Resources => SYS_INSPECT_RESOURCES_TYPE_ID,
        InspectProjection::StateCells => SYS_INSPECT_STATE_CELLS_TYPE_ID,
        InspectProjection::UiNodes => SYS_INSPECT_UI_NODES_TYPE_ID,
        InspectProjection::PresentationCandidates => SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
        InspectProjection::RuntimeBindings => SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
        InspectProjection::SecurityDecisions => SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
    }
}

#[cfg(test)]
pub(super) fn inspect_target_is_observer(
    context: ClientExecutionContext,
    target: InvocationId,
) -> bool {
    inspect_target_is_observer_with_lineage(ObserverLineage::compatibility(context), target)
}

fn inspect_target_is_observer_with_lineage(lineage: ObserverLineage, target: InvocationId) -> bool {
    lineage.contains(target)
}

pub(crate) fn inspect_invocation_target(value: &RuntimeValue) -> Option<InvocationId> {
    let RuntimeValue::Reference { target, object } = value else {
        return None;
    };
    if *target != SYS_INSPECT_INVOCATION_TYPE_ID || object.to_bytes() == [0; 16] {
        return None;
    }
    Some(InvocationId::from_bytes(object.to_bytes()))
}

const INSPECT_SNAPSHOT_ROW_TAG: u8 = 1;

pub(super) fn decode_inspect_snapshot_target_row(
    row: &[u8],
    epoch_id: InspectEpochId,
) -> Result<InvocationId, ClientInspectError> {
    if row.len() < 68 || row[0] != INSPECT_SNAPSHOT_ROW_TAG || row[1..9] != [0; 8] {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    if InspectEpochId::from_bytes(row[9..25].try_into().expect("snapshot epoch width")) != epoch_id
    {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    let target = InvocationId::from_bytes(row[25..41].try_into().expect("snapshot target width"));
    if target.to_bytes() == [0; 16] {
        return Err(inspect_carrier_error("inspect.invalid_target"));
    }
    let mut offset = 57;
    let outcome = *row
        .get(offset)
        .ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
    if !(1..=4).contains(&outcome) {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    offset += 1 + 8;
    let result = *row
        .get(offset)
        .ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
    offset += 1;
    match result {
        0 => {}
        1 => {
            let value_count = row
                .get(offset..)
                .and_then(|bytes| bytes.get(..8))
                .and_then(|bytes| bytes.try_into().ok())
                .map(u64::from_be_bytes)
                .ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
            if value_count == 0 {
                return Err(inspect_carrier_error("inspect.malformed_carrier"));
            }
            offset += 8;
        }
        _ => return Err(inspect_carrier_error("inspect.malformed_carrier")),
    }
    let duration = *row
        .get(offset)
        .ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
    offset += 1;
    match duration {
        0 => {}
        1 => offset += 8,
        _ => return Err(inspect_carrier_error("inspect.malformed_carrier")),
    }
    if offset != row.len() {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    Ok(target)
}

pub(super) fn inspect_snapshot_target_from_envelope(
    active: &ActiveDatabaseRevision,
    envelope: &InspectCarrierEnvelope,
) -> Result<InvocationId, ClientInspectError> {
    if envelope.carrier_kind() != InspectCarrierKind::Snapshot || envelope.rows().len() != 1 {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Err(inspect_carrier_error("inspect.projection_failed"));
    };
    let registry = registered_opaque_codecs(standard)
        .map_err(|_| inspect_carrier_error("inspect.projection_failed"))?;
    let row = decode_constructed_value(active, &registry, &envelope.rows()[0])
        .map_err(|_| inspect_carrier_error("inspect.malformed_carrier"))?;
    let RuntimeValue::Constructed(constructed) = row else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    let TypeDescriptorKind::List(child) = constructed.descriptor().kind() else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    if child.kind() != TypeDescriptorKind::Named(BINARY_LARGE_OBJECT_TYPE_ID) {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    let ConstructedValueKind::List(values) = constructed.kind() else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    // Encoded root_target bytes are checked against the authenticated
    // AuthenticatedInspectSnapshot on the server. This client decoder only has
    // the opaque envelope and no authenticated FunctionId root context, so the
    // server remains authoritative for that binding.
    decode_inspect_snapshot_target_row(payload, envelope.epoch_id())
}

pub(super) fn inspect_carrier_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: TypeId,
) -> bool {
    decode_inspect_carrier(active, value, expected).is_ok()
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub(super) fn evaluate_inspect_expression(
    active: &ActiveDatabaseRevision,
    operation: &InspectOperationNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    if context.pair() != active.pair() {
        return Err(ClientExecutionError::Inspect {
            context,
            source: ClientInspectError::RevisionMismatch {
                expected: active.pair(),
                actual: context.pair(),
            },
        });
    }
    if depth > orna_artifact::client_plan::MAX_EXPRESSION_DEPTH {
        return Err(ClientExecutionError::Inspect {
            context,
            source: ClientInspectError::RecursionLimit,
        });
    }
    let mut snapshot_epoch_id = None;
    let mut snapshot_envelope_for_projection = None;
    let target_invocation_id;
    let mut snapshot_options = None;
    let operation = match operation {
        InspectOperationNode::Snapshot { target, options } => {
            if options.is_some() {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: ClientInspectError::Failed(stable_inspect_provider_error(
                        "inspect.invalid_options",
                    )),
                });
            }
            let target = evaluate_expression_with_fuel(
                active,
                target,
                context,
                &lineage,
                arguments,
                declarations,
                grants,
                state,
                depth + 1,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            let Some(invocation) = inspect_invocation_target(&target) else {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: ClientInspectError::InvalidTarget,
                });
            };
            if inspect_target_is_observer_with_lineage(lineage, invocation) {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: inspect_carrier_error("inspect.recursion"),
                });
            }
            if let Some(options) = options {
                let options = evaluate_expression_with_fuel(
                    active,
                    options,
                    context,
                    &lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth + 1,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )?;
                if !runtime_value_matches(
                    active,
                    &options,
                    ResolvedType::Named(SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID),
                ) {
                    return Err(ClientExecutionError::Inspect {
                        context,
                        source: ClientInspectError::InvalidSnapshot,
                    });
                }
                snapshot_options = Some(options);
            }
            target_invocation_id = Some(invocation);
            ClientInspectOperation::Snapshot { target }
        }
        InspectOperationNode::Projection {
            projection,
            snapshot,
        } => {
            let snapshot = evaluate_expression_with_fuel(
                active,
                snapshot,
                context,
                &lineage,
                arguments,
                declarations,
                grants,
                state,
                depth + 1,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            let snapshot_envelope =
                match decode_inspect_carrier(active, &snapshot, SYS_INSPECT_SNAPSHOT_TYPE_ID) {
                    Ok(envelope) => envelope,
                    Err(source) => {
                        return Err(ClientExecutionError::Inspect { context, source });
                    }
                };
            let invocation = inspect_snapshot_target_from_envelope(active, &snapshot_envelope)
                .map_err(|source| ClientExecutionError::Inspect { context, source })?;
            if inspect_target_is_observer_with_lineage(lineage, invocation) {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: inspect_carrier_error("inspect.recursion"),
                });
            }
            target_invocation_id = Some(invocation);
            snapshot_epoch_id = Some(snapshot_envelope.epoch_id());
            snapshot_envelope_for_projection = Some(snapshot_envelope);
            ClientInspectOperation::Projection {
                projection: *projection,
                snapshot,
            }
        }
    };
    let Some(executor) = executor.as_deref_mut() else {
        return Err(ClientExecutionError::Inspect {
            context,
            source: ClientInspectError::Failed("inspect.runtime_unavailable".to_owned()),
        });
    };
    let request = match (target_invocation_id, snapshot_options) {
        (Some(target), Some(options)) => ClientInspectRequest::with_target_invocation_and_options(
            context,
            operation.clone(),
            target,
            options,
            lineage,
        ),
        (Some(target), None) => ClientInspectRequest::with_target_invocation(
            context,
            operation.clone(),
            target,
            lineage,
        ),
        (None, None) => {
            ClientInspectRequest::with_provenance(context, operation.clone(), None, None, lineage)
        }
        (None, Some(_)) => unreachable!("snapshot options require a target"),
    };
    let value = executor
        .inspect(request)
        .map_err(|code| ClientExecutionError::Inspect {
            context,
            source: ClientInspectError::Failed(stable_inspect_provider_error(&code)),
        })?;
    let expected = match operation {
        ClientInspectOperation::Snapshot { .. } => SYS_INSPECT_SNAPSHOT_TYPE_ID,
        ClientInspectOperation::Projection { projection, .. } => {
            inspect_projection_result_type(projection)
        }
    };
    let envelope = match decode_inspect_carrier(active, &value, expected) {
        Ok(envelope) => envelope,
        Err(source) => {
            return Err(ClientExecutionError::Inspect { context, source });
        }
    };
    if snapshot_epoch_id.is_some_and(|epoch_id| epoch_id != envelope.epoch_id()) {
        return Err(ClientExecutionError::Inspect {
            context,
            source: inspect_carrier_error("inspect.epoch_mismatch"),
        });
    }
    if let Some(expected_target) = target_invocation_id {
        match operation {
            ClientInspectOperation::Snapshot { .. } => {
                let actual_target = inspect_snapshot_target_from_envelope(active, &envelope)
                    .map_err(|source| ClientExecutionError::Inspect { context, source })?;
                if actual_target != expected_target {
                    return Err(ClientExecutionError::Inspect {
                        context,
                        source: inspect_carrier_error("inspect.epoch_mismatch"),
                    });
                }
            }
            ClientInspectOperation::Projection { projection, .. } => {
                let snapshot = snapshot_envelope_for_projection
                    .as_ref()
                    .expect("projection operations retain their decoded snapshot");
                inspect_carrier_matches_snapshot(
                    active,
                    snapshot,
                    expected_target,
                    InspectCarrierKind::from_type_id(inspect_projection_result_type(projection))
                        .expect("sealed projection type must map to a carrier"),
                    &envelope,
                )
                .map_err(|source| ClientExecutionError::Inspect { context, source })?;
            }
        }
    }
    Ok(value)
}
