//! USER-state root, declaration, type, and codec validation.

use super::*;

/// Rebinds a retained session to the active security snapshot before USER-state access.
///
/// The returned session is local to this operation; the caller-owned binding
/// remains opaque and is never replaced or exposed. A disabled principal or
/// revoked selected role fails before any state query, write, or audit append.
pub(super) fn revalidate_authenticated_session(
    security: &SecuritySnapshot,
    authenticated_session: &AuthenticatedSession,
    active_pair: RevisionPair,
    function: FunctionId,
) -> Result<AuthenticatedSession, PostgresKernelError> {
    security
        .bind_authenticated_session(
            authenticated_session.principal(),
            authenticated_session.active_roles().to_vec(),
        )
        .map_err(|_| PostgresKernelError::StateExecuteDenied {
            pair: active_pair,
            function,
            reason: ExecuteDenial::InvalidSession,
        })
}

/// Rejects a caller-supplied root that is not an active CLIENT identity.
///
/// USER-state storage is keyed by the root function, so the root must be
/// resolved against the same active catalogue snapshot as its state slots.
/// This guard deliberately runs before any state query, write, or allowed
/// USER-state audit append.
pub(super) fn validate_active_user_state_root(
    active: &ActiveDatabaseRevision,
    root_function: FunctionId,
) -> Result<(), PostgresKernelError> {
    let definition = validate_user_state_root_definition(
        root_function,
        active.catalogue().function_by_id(root_function),
    )?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == root_function && revision.id() == definition.current_revision()
        })
        .ok_or_else(|| PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{root_function:?}"),
            rule: "USER state root must have its active CLIENT function revision",
        })?;
    if revision.artifact().kind() != ExecutableArtifactKind::Client {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{root_function:?}"),
            rule: "USER state root must carry an active CLIENT function artifact",
        });
    }
    Ok(())
}

pub(super) fn validate_user_state_root_definition(
    root_function: FunctionId,
    definition: Option<&FunctionDefinition>,
) -> Result<&FunctionDefinition, PostgresKernelError> {
    let definition = definition.ok_or_else(|| PostgresKernelError::DurableInvariant {
        relation: STATE_RELATION,
        record: format!("{root_function:?}"),
        rule: "USER state root must identify an active CLIENT function",
    })?;
    if definition.id() != root_function {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{root_function:?}"),
            rule: "USER state root catalogue definition must match its supplied identity",
        });
    }
    if definition.domain() != FunctionDomain::Client {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{root_function:?}"),
            rule: "USER state root must be a CLIENT function",
        });
    }
    Ok(definition)
}

pub(super) fn require_expected_type(
    cell: &UserStateCell,
    expected_types: &BTreeMap<(FunctionId, StateSlotId), TypeId>,
) -> Result<(), PostgresKernelError> {
    let Some(expected) = expected_types.get(&(cell.key().function(), cell.key().state_slot()))
    else {
        return Ok(());
    };
    if cell_type_matches(cell, *expected) {
        return Ok(());
    }
    let change = UserStateChange::new(
        cell.key().root_function(),
        cell.key().state_profile().to_owned(),
        cell.key().function(),
        cell.key().instance_key().to_owned(),
        cell.key().state_slot(),
        Some(cell.revision()),
        cell.value().clone(),
        *expected,
    )
    .map_err(PostgresKernelError::UserState)?;
    match apply_change(Some(cell), &change, cell.key().principal()) {
        Err(error) => Err(PostgresKernelError::UserState(error)),
        Ok(_) => unreachable!("a mismatched expected type must fail closed"),
    }
}

pub(super) fn require_declared_user_state_type(
    key: UserStateKeyWithoutPrincipal,
    expected_type: TypeId,
    current_type: TypeId,
) -> Result<(), PostgresKernelError> {
    if expected_type == current_type {
        return Ok(());
    }
    Err(PostgresKernelError::UserState(
        UserStateError::TypeIncompatible {
            key: Box::new(key),
            expected: expected_type,
            current: current_type,
        },
    ))
}

pub(super) fn active_user_state_slot_type(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    state_slot: StateSlotId,
) -> Result<TypeId, PostgresKernelError> {
    let definition = active.catalogue().function_by_id(function).ok_or_else(|| {
        PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{function:?}"),
            rule: "USER state slot must identify an active CLIENT function",
        }
    })?;
    if definition.domain() != FunctionDomain::Client {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{function:?}"),
            rule: "USER state slot owner must be a CLIENT function",
        });
    }
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function && revision.id() == definition.current_revision()
        })
        .ok_or_else(|| PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{function:?}"),
            rule: "USER state slot owner must have its active CLIENT function revision",
        })?;
    let plan = decode_active_client_state_plan(revision)?;
    declared_user_state_slot(revision.function(), function, state_slot, &plan)
}

pub(super) fn decode_active_client_state_plan(
    revision: &FunctionRevisionRecord,
) -> Result<StateClientPlan, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Client || artifact.format() != CLIENT_PLAN_FORMAT
    {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{:?}", revision.function()),
            rule: "active CLIENT USER state owner must carry an orna.client-plan artifact",
        });
    }
    match artifact.version() {
        STATE_FORMAT_VERSION => StateClientPlan::decode(artifact.payload()).map_err(|_| {
            PostgresKernelError::DurableInvariant {
                relation: STATE_RELATION,
                record: format!("{:?}", revision.function()),
                rule: "active CLIENT USER state plan must decode as a version-four state plan",
            }
        }),
        CAPABILITY_FORMAT_VERSION => {
            let plan = CapabilityClientPlan::decode(artifact.payload()).map_err(|_| {
                PostgresKernelError::DurableInvariant {
                    relation: STATE_RELATION,
                    record: format!("{:?}", revision.function()),
                    rule: "active CLIENT capability state plan must decode canonically",
                }
            })?;
            match plan.inner_plan() {
                InnerClientPlan::State(state) => Ok(state.clone()),
                _ => Err(PostgresKernelError::DurableInvariant {
                    relation: STATE_RELATION,
                    record: format!("{:?}", revision.function()),
                    rule: "active CLIENT USER state owner must carry a state plan",
                }),
            }
        }
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: format!("{:?}", revision.function()),
            rule: "active CLIENT USER state owner must carry a supported state plan",
        }),
    }
}

pub(super) fn declared_user_state_slot(
    owner_function: FunctionId,
    function: FunctionId,
    state_slot: StateSlotId,
    plan: &StateClientPlan,
) -> Result<TypeId, PostgresKernelError> {
    let record = format!("owner={owner_function:?}, function={function:?}, slot={state_slot:?}");
    if owner_function != function {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record,
            rule: "USER state slot must be presented with its owning CLIENT function",
        });
    }
    let Some(slot) = plan
        .slots()
        .iter()
        .find(|slot| slot.state_slot_id() == state_slot)
    else {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record,
            rule: "USER state slot must be declared by its owning CLIENT function",
        });
    };
    if slot.scope() != StateScope::User {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record,
            rule: "USER state service cannot access LOCAL or SESSION CLIENT state slots",
        });
    }
    Ok(slot.type_id())
}

#[cfg(test)]
pub(super) fn validate_user_state_slot_declaration(
    owner_function: FunctionId,
    function: FunctionId,
    state_slot: StateSlotId,
    value_type: TypeId,
    plan: &StateClientPlan,
) -> Result<(), PostgresKernelError> {
    let declared_type = declared_user_state_slot(owner_function, function, state_slot, plan)?;
    let key = UserStateKeyWithoutPrincipal::new(
        owner_function,
        String::new(),
        function,
        String::new(),
        state_slot,
    )
    .map_err(PostgresKernelError::UserState)?;
    require_declared_user_state_type(key, value_type, declared_type)
}

pub(super) fn reject_sealed_inspect_state_type(
    value_type: TypeId,
    record: impl Into<String>,
) -> Result<(), PostgresKernelError> {
    if is_sealed_inspect_type_id(value_type) {
        return Err(PostgresKernelError::DurableInvariant {
            relation: STATE_RELATION,
            record: record.into(),
            rule: "USER state cannot persist sealed Inspector carrier type identities",
        });
    }
    Ok(())
}

pub(super) fn state_value_registry(
    active: &ActiveDatabaseRevision,
) -> Result<OpaqueCodecRegistry, PostgresKernelError> {
    let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.active_revision",
            record: active.pair().catalogue().canonical(),
            rule: "USER state requires the accepted verified standard snapshot",
        }
    })?;
    registered_opaque_codecs(standard).map_err(|_| PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.standard_library_revisions",
        record: standard.revision().canonical(),
        rule: "the verified standard snapshot must bind its opaque codec registry",
    })
}

pub(super) fn validate_state_profile(state_profile: &str) -> Result<(), PostgresKernelError> {
    UserStateKeyWithoutPrincipal::new(
        FunctionId::from_bytes([0; 16]),
        state_profile.to_owned(),
        FunctionId::from_bytes([0; 16]),
        String::new(),
        StateSlotId::from_bytes([0; 16]),
    )
    .map(|_| ())
    .map_err(PostgresKernelError::UserState)
}
