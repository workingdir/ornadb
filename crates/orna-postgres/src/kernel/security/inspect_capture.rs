use super::*;

/// Captures the structural epoch for one completed authenticated resource.
///
/// Resource requests do not carry an independent client offer. The nested
/// invocation therefore records no runtime-binding rows, while the immutable
/// invocation and trace carriers remain available to the Inspector.
pub(super) async fn capture_completed_resource_inspect_snapshot(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authenticated_session: &AuthenticatedSession,
    invocation: InvocationId,
    root_target: Option<InvocationTarget>,
) -> Result<InspectEpochId, PostgresKernelError> {
    let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.active_revision",
            record: active.pair().catalogue().canonical(),
            rule: "completed resource capture requires the verified standard snapshot",
        }
    })?;
    let registry =
        registered_opaque_codecs(standard).map_err(|_| PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.standard_library_revisions",
            record: standard.revision().canonical(),
            rule: "completed resource capture requires the verified codec registry",
        })?;
    let root_target = root_target.ok_or_else(|| {
        sealed_target_invariant(active, "completed resource producer must retain its target")
    })?;
    let events = sealed_completed_events_from_values(
        authenticated_session.principal(),
        invocation,
        Vec::new(),
    )?;
    crate::inspect::capture_inspect_snapshot_in_transaction(
        transaction,
        active,
        &registry,
        authenticated_session,
        invocation,
        InspectSnapshotOptions::structural(),
        authenticated_session.principal(),
        root_target.function(),
        InspectOutcomeKind::Allowed,
        &events,
        None,
        None,
        None,
        None,
    )
    .await
}

/// Captures one inspection epoch and its trace rows for a completed sealed
/// invocation in the caller's protected transaction.
///
/// ADR 0064 wires capture into the sealed dispatch: after the protected
/// decision and before/at execution, the produced Event batch becomes the
/// durable trace rows and one immutable snapshot epoch. v1 retains typed
/// state values in the protected epoch so a later `INSPECT VALUES` projection
/// can reveal them without reading mutable USER state; the projection still
/// redacts them unless the classifier is granted. Denied, bind-failed, and
/// presentation-failed invocations produce no Event batch and therefore no
/// epoch.
#[allow(clippy::too_many_arguments)]
pub(super) async fn capture_sealed_invocation_snapshot(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    authenticated_session: &AuthenticatedSession,
    invocation: InvocationId,
    root_target: FunctionId,
    events: &InvocationEventBatch,
    client_offer: &InvocationClientOffer,
    loaded_user_state_cells: Option<&[UserStateCell]>,
    output_requirement: Option<&InvocationOutputRequirement>,
) -> Result<InspectEpochId, PostgresKernelError> {
    crate::inspect::capture_inspect_snapshot_in_transaction(
        transaction,
        active,
        registry,
        authenticated_session,
        invocation,
        // Retain typed values in the immutable epoch. The installed
        // projection applies the independent `Values` classifier.
        InspectSnapshotOptions::new(true, false, false, false),
        authenticated_session.principal(),
        root_target,
        InspectOutcomeKind::Allowed,
        events,
        Some(client_offer),
        None,
        loaded_user_state_cells,
        output_requirement,
    )
    .await
}
