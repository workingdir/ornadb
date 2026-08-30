use super::*;
use orna_artifact::client_plan::{ClientExpressionNode, StateDefault, StateSlot};
use orna_core::{
    CatalogueRevisionId, FunctionRevisionId, PrincipalId, SourceRevisionId, TypeId,
    catalogue::{
        FunctionDefinition, FunctionReturn, FunctionSecurity, FunctionTransaction,
        FunctionVolatility, QualifiedSemanticName,
    },
    revision::RevisionPair,
    security::{Principal, PrincipalKind, PrincipalStatus, RoleMembership},
    types::{ResolvedType, StandardScalar},
    value::RuntimeValue,
};

const PRINCIPAL: orna_core::PrincipalId = PrincipalId::from_bytes([0x11; 16]);
const OTHER_PRINCIPAL: orna_core::PrincipalId = PrincipalId::from_bytes([0x22; 16]);
const ROOT: FunctionId = FunctionId::from_bytes([0x31; 16]);
const FUNCTION: FunctionId = FunctionId::from_bytes([0x32; 16]);
const OTHER_FUNCTION: FunctionId = FunctionId::from_bytes([0x36; 16]);
const SLOT: StateSlotId = StateSlotId::from_bytes([0x33; 16]);
const OTHER_SLOT: StateSlotId = StateSlotId::from_bytes([0x37; 16]);
const INTEGER: TypeId = TypeId::from_bytes([0x34; 16]);
const TEXT: TypeId = TypeId::from_bytes([0x35; 16]);

fn change(expected_revision: Option<u64>, value: i64) -> UserStateChange {
    change_for_instance(String::new(), expected_revision, value)
}

fn change_for_instance(
    instance_key: String,
    expected_revision: Option<u64>,
    value: i64,
) -> UserStateChange {
    UserStateChange::new(
        ROOT,
        String::new(),
        FUNCTION,
        instance_key,
        SLOT,
        expected_revision,
        RuntimeValue::BigInt(value),
        INTEGER,
    )
    .expect("test change is valid")
}

fn cell(principal: PrincipalId, revision: u64, value: i64) -> UserStateCell {
    cell_for_instance(principal, String::new(), revision, value)
}

fn cell_for_instance(
    principal: PrincipalId,
    instance_key: String,
    revision: u64,
    value: i64,
) -> UserStateCell {
    UserStateCell::new(
        UserStateKey::new(principal, ROOT, String::new(), FUNCTION, instance_key, SLOT)
            .expect("test key is valid"),
        RuntimeValue::BigInt(value),
        INTEGER,
        revision,
        SystemTime::UNIX_EPOCH,
    )
}

fn state_plan(scope: StateScope, value_type: TypeId) -> StateClientPlan {
    StateClientPlan::new(
        ClientExpressionNode::Boolean { value: true },
        vec![StateSlot::new(SLOT, value_type, scope, StateDefault::Unset)],
    )
}

fn root_definition(domain: FunctionDomain) -> FunctionDefinition {
    FunctionDefinition::new(
        ROOT,
        QualifiedSemanticName::new(["test", "root"]).expect("test root name"),
        domain,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionRevisionId::from_bytes([0x38; 16]),
        FunctionSecurity::Invoker,
        (domain == FunctionDomain::Server).then_some(FunctionTransaction::ReadOnly),
        if domain == FunctionDomain::Client {
            FunctionVolatility::Immutable
        } else {
            FunctionVolatility::Stable
        },
    )
}

#[test]
fn unknown_user_state_root_is_rejected() {
    let error = validate_user_state_root_definition(FUNCTION, None)
        .expect_err("an unknown USER-state root must fail closed");
    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            rule: "USER state root must identify an active CLIENT function",
            ..
        }
    ));
}

#[test]
fn inactive_user_state_root_is_rejected() {
    let error = validate_user_state_root_definition(OTHER_FUNCTION, None)
        .expect_err("an inactive USER-state root must fail closed");
    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            rule: "USER state root must identify an active CLIENT function",
            ..
        }
    ));
}

#[test]
fn server_user_state_root_is_rejected() {
    let definition = root_definition(FunctionDomain::Server);
    let error = validate_user_state_root_definition(ROOT, Some(&definition))
        .expect_err("a SERVER USER-state root must fail closed");
    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            rule: "USER state root must be a CLIENT function",
            ..
        }
    ));
}

#[test]
fn active_client_user_state_root_is_accepted() {
    let definition = root_definition(FunctionDomain::Client);
    assert!(validate_user_state_root_definition(ROOT, Some(&definition)).is_ok());
}

#[test]
fn mismatched_user_state_root_definition_is_rejected() {
    let definition = root_definition(FunctionDomain::Client);
    let error = validate_user_state_root_definition(FUNCTION, Some(&definition))
        .expect_err("a catalogue definition for another identity must fail closed");
    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            rule: "USER state root catalogue definition must match its supplied identity",
            ..
        }
    ));
}

#[test]
fn retained_session_rebinds_or_denies_before_state_access() {
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x41; 16]),
        CatalogueRevisionId::from_bytes([0x42; 16]),
    );
    let active = SecuritySnapshot::new(
        pair,
        vec![],
        vec![Principal::new(
            PRINCIPAL,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    )
    .expect("active state security snapshot must validate");
    let session = active
        .bind_authenticated_session(PRINCIPAL, vec![])
        .expect("active state session must bind");
    let rebound = revalidate_authenticated_session(
        &active,
        &session,
        pair,
        SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
    )
    .expect("active retained state session must rebind");
    assert_eq!(rebound.principal(), PRINCIPAL);

    let disabled = SecuritySnapshot::new(
        pair,
        vec![],
        vec![Principal::new(
            PRINCIPAL,
            PrincipalKind::User,
            PrincipalStatus::Disabled,
        )],
        vec![],
        vec![],
    )
    .expect("disabled state security snapshot must validate");
    let error = revalidate_authenticated_session(
        &disabled,
        &session,
        pair,
        SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
    )
    .expect_err("disabled retained state session must be rejected");
    assert!(matches!(
        error,
        PostgresKernelError::StateExecuteDenied {
            function: SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
            reason: ExecuteDenial::InvalidSession,
            ..
        }
    ));

    let role = PrincipalId::from_bytes([0x43; 16]);
    let role_active = SecuritySnapshot::new(
        pair,
        vec![],
        vec![
            Principal::new(PRINCIPAL, PrincipalKind::User, PrincipalStatus::Active),
            Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
        ],
        vec![RoleMembership::new(role, PRINCIPAL)],
        vec![],
    )
    .expect("role-bound state security snapshot must validate");
    let role_session = role_active
        .bind_authenticated_session(PRINCIPAL, vec![role])
        .expect("selected role state session must bind");
    let revoked = SecuritySnapshot::new(
        pair,
        vec![],
        vec![Principal::new(
            PRINCIPAL,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    )
    .expect("revoked-role state security snapshot must validate");
    let error = revalidate_authenticated_session(
        &revoked,
        &role_session,
        pair,
        SYS_STATE_WRITE_USER_STATE_FUNCTION_ID,
    )
    .expect_err("revoked selected role must invalidate retained state session");
    assert!(matches!(
        error,
        PostgresKernelError::StateExecuteDenied {
            function: SYS_STATE_WRITE_USER_STATE_FUNCTION_ID,
            reason: ExecuteDenial::InvalidSession,
            ..
        }
    ));
}

#[test]
fn undeclared_user_slot_is_rejected() {
    let plan = state_plan(StateScope::User, INTEGER);
    let error =
        validate_user_state_slot_declaration(FUNCTION, FUNCTION, OTHER_SLOT, INTEGER, &plan)
            .expect_err("unknown USER state slot must fail closed");
    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            rule: "USER state slot must be declared by its owning CLIENT function",
            ..
        }
    ));
}

#[test]
fn user_slot_presented_with_wrong_owner_is_rejected() {
    let plan = state_plan(StateScope::User, INTEGER);
    let error =
        validate_user_state_slot_declaration(FUNCTION, OTHER_FUNCTION, SLOT, INTEGER, &plan)
            .expect_err("USER state slot must use its owning function");
    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant {
            rule: "USER state slot must be presented with its owning CLIENT function",
            ..
        }
    ));
}

#[test]
fn local_and_session_slots_are_rejected_by_user_service() {
    for scope in [StateScope::Local, StateScope::Session] {
        let plan = state_plan(scope, INTEGER);
        let error = validate_user_state_slot_declaration(FUNCTION, FUNCTION, SLOT, INTEGER, &plan)
            .expect_err("non-USER state scope must fail closed");
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                rule: "USER state service cannot access LOCAL or SESSION CLIENT state slots",
                ..
            }
        ));
    }
}

#[test]
fn user_slot_type_mismatch_is_rejected_against_active_plan() {
    let plan = state_plan(StateScope::User, INTEGER);
    let error = validate_user_state_slot_declaration(FUNCTION, FUNCTION, SLOT, TEXT, &plan)
        .expect_err("USER state type must match the active declaration");
    let message = error.to_string();
    match error {
        PostgresKernelError::UserState(UserStateError::TypeIncompatible {
            expected,
            current,
            ..
        }) => {
            assert_eq!(expected, TEXT);
            assert_eq!(current, INTEGER);
        }
        other => panic!("expected ORNA0901 type mismatch, got {other:?}"),
    }
    assert!(message.contains("ORNA0901"));
}

#[test]
fn load_declared_type_is_expected_and_persisted_type_is_current() {
    let key = UserStateKeyWithoutPrincipal::new(ROOT, String::new(), FUNCTION, String::new(), SLOT)
        .expect("test key is valid");
    let error = require_declared_user_state_type(key, INTEGER, TEXT)
        .expect_err("declared and persisted USER state types must agree");
    let message = error.to_string();
    match error {
        PostgresKernelError::UserState(UserStateError::TypeIncompatible {
            expected,
            current,
            ..
        }) => {
            assert_eq!(expected, INTEGER);
            assert_eq!(current, TEXT);
        }
        other => panic!("expected ORNA0901 type mismatch, got {other:?}"),
    }
    assert!(message.contains("ORNA0901"));
}

#[test]
fn first_write_and_matching_revision_increment() {
    let first = apply_change(None, &change(None, 1), PRINCIPAL).expect("first write succeeds");
    assert_eq!(
        first.outcome(),
        UserStateWriteOutcome::Written { revision: 1 }
    );
    let current = cell(PRINCIPAL, 1, 1);
    let second = apply_change(Some(&current), &change(Some(1), 2), PRINCIPAL)
        .expect("matching revision succeeds");
    assert_eq!(
        second.outcome(),
        UserStateWriteOutcome::Written { revision: 2 }
    );
}

#[test]
fn stale_revision_is_a_per_change_conflict_with_current_revision() {
    let current = cell(PRINCIPAL, 3, 1);
    let error = apply_change(Some(&current), &change(Some(2), 2), PRINCIPAL)
        .expect_err("stale revision must fail closed");
    assert_eq!(error.code(), Some("ORNA0902"));
    let result = UserStateWriteResult::new(
        change(Some(2), 2).key_without_principal(),
        UserStateWriteOutcome::Conflict {
            current_revision: 3,
        },
    );
    assert_eq!(
        result.outcome(),
        UserStateWriteOutcome::Conflict {
            current_revision: 3
        }
    );
}

#[test]
fn type_mismatch_fails_load_and_write_closed_with_orna0901() {
    let current = cell(PRINCIPAL, 1, 1);
    let different_type = TypeId::from_bytes([0x35; 16]);
    let change = UserStateChange::new(
        ROOT,
        String::new(),
        FUNCTION,
        String::new(),
        SLOT,
        Some(1),
        RuntimeValue::BigInt(2),
        different_type,
    )
    .expect("test change is valid");
    let write_error = apply_change(Some(&current), &change, PRINCIPAL)
        .expect_err("type mismatch must fail closed");
    assert_eq!(write_error.code(), Some("ORNA0901"));

    let load_error = require_expected_type(
        &current,
        &BTreeMap::from([((FUNCTION, SLOT), different_type)]),
    )
    .expect_err("load mismatch must fail closed");
    assert!(matches!(load_error, PostgresKernelError::UserState(_)));
    assert!(load_error.to_string().contains("ORNA0901"));
}

#[test]
fn principal_is_derived_from_session_not_the_change() {
    let current = cell(OTHER_PRINCIPAL, 1, 1);
    let error = apply_change(Some(&current), &change(Some(1), 2), PRINCIPAL)
        .expect_err("cross-principal cell must fail closed");
    assert_eq!(error.code(), Some("ORNA0903"));
}

#[test]
fn sealed_inspector_state_types_are_rejected_but_scalars_are_allowed() {
    let sealed_types = [
        orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID,
        orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
        orna_core::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
        orna_core::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID,
        orna_core::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
        orna_core::system::SYS_INSPECT_CALLS_TYPE_ID,
        orna_core::system::SYS_INSPECT_RESOURCES_TYPE_ID,
        orna_core::system::SYS_INSPECT_STATE_CELLS_TYPE_ID,
        orna_core::system::SYS_INSPECT_UI_NODES_TYPE_ID,
        orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
        orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
        orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
    ];
    for sealed_type in sealed_types {
        let error = reject_sealed_inspect_state_type(sealed_type, "forged row")
            .expect_err("sealed Inspector identities must fail closed");
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                relation: STATE_RELATION,
                ..
            }
        ));
    }
    reject_sealed_inspect_state_type(INTEGER, "ordinary scalar")
        .expect("ordinary scalar USER state remains persistable");
}

#[test]
fn forged_sealed_inspector_cell_aborts_the_write_plan() {
    let current = UserStateCell::new(
        UserStateKey::new(
            PRINCIPAL,
            ROOT,
            String::new(),
            FUNCTION,
            String::new(),
            SLOT,
        )
        .expect("test key is valid"),
        RuntimeValue::BigInt(1),
        orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
        1,
        SystemTime::UNIX_EPOCH,
    );
    let change = change(Some(1), 2);
    let key = change.key_without_principal().with_principal(PRINCIPAL);
    let mut current_cells = HashMap::from([(key, Some(current))]);
    let error = plan_user_state_changes(&[change], PRINCIPAL, &mut current_cells)
        .expect_err("a forged persisted Inspector identity must fail closed");
    assert!(matches!(error, PostgresKernelError::UserState(_)));
}

#[test]
fn expected_type_match_and_instance_requests_are_closed() {
    let current = cell(PRINCIPAL, 1, 1);
    require_expected_type(&current, &BTreeMap::from([((FUNCTION, SLOT), INTEGER)]))
        .expect("matching type loads");
    assert!(UserStateInstanceRequest::new(FUNCTION, String::new()).is_ok());
    assert!(UserStateInstanceRequest::new(FUNCTION, "bad\0key".to_owned()).is_err());
}

#[test]
fn empty_instance_request_selects_only_the_default_persisted_instance() {
    let requests = requested_state_instances(&[]);
    let default_cell = cell(PRINCIPAL, 1, 1);
    let named_cell = cell_for_instance(PRINCIPAL, "named".to_owned(), 1, 2);

    assert!(state_instance_is_requested(
        &[],
        &requests,
        default_cell.key().function(),
        default_cell.key().instance_key()
    ));
    assert!(!state_instance_is_requested(
        &[],
        &requests,
        named_cell.key().function(),
        named_cell.key().instance_key()
    ));
}

#[test]
fn explicit_instance_request_still_selects_the_named_persisted_instance() {
    let explicit = UserStateInstanceRequest::new(FUNCTION, "named".to_owned())
        .expect("explicit instance request is valid");
    let requests = requested_state_instances(std::slice::from_ref(&explicit));
    let named_cell = cell_for_instance(PRINCIPAL, "named".to_owned(), 1, 2);

    assert!(state_instance_is_requested(
        std::slice::from_ref(&explicit),
        &requests,
        named_cell.key().function(),
        named_cell.key().instance_key(),
    ));
}

#[test]
fn mixed_batch_conflict_returns_all_conflicts_without_staging_writes() {
    let default_cell = cell(PRINCIPAL, 3, 1);
    let named_cell = cell_for_instance(PRINCIPAL, "named".to_owned(), 3, 2);
    let first = change(Some(3), 10);
    let second = change_for_instance("named".to_owned(), Some(2), 20);
    let default_key = first.key_without_principal().with_principal(PRINCIPAL);
    let named_key = second.key_without_principal().with_principal(PRINCIPAL);
    let original_default = default_cell.clone();
    let original_named = named_cell.clone();
    let mut current_cells = HashMap::from([
        (default_key.clone(), Some(default_cell)),
        (named_key.clone(), Some(named_cell)),
    ]);
    let (results, pending) =
        plan_user_state_changes(&[first, second], PRINCIPAL, &mut current_cells)
            .expect("conflicts are returned as closed results");

    assert_eq!(pending.len(), 0);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| {
        result.outcome()
            == UserStateWriteOutcome::Conflict {
                current_revision: 3,
            }
    }));
    assert_eq!(
        current_cells.get(&default_key),
        Some(&Some(original_default))
    );
    assert_eq!(current_cells.get(&named_key), Some(&Some(original_named)));
}

#[test]
fn duplicate_keys_fail_preflight_without_staging_or_persistence_plan() {
    let current = cell(PRINCIPAL, 1, 1);
    let key = current.key().clone();
    let original = current.clone();
    let first = change(Some(1), 2);
    let second = change(Some(1), 3);
    let mut current_cells = HashMap::from([(key, Some(current))]);

    let error = plan_user_state_changes(&[first, second], PRINCIPAL, &mut current_cells)
        .expect_err("duplicate USER state keys must fail before planning");

    assert!(matches!(
        error,
        PostgresKernelError::UserState(UserStateError::InvalidChange { .. })
    ));
    assert!(error.to_string().contains("duplicate key"));
    assert_eq!(current_cells.values().next(), Some(&Some(original)));
}

#[test]
fn hard_failure_returns_no_commit_plan_for_prior_success() {
    let current = cell(PRINCIPAL, 1, 1);
    let second_current = cell_for_instance(PRINCIPAL, "other".to_owned(), 1, 1);
    let first = change(Some(1), 2);
    let second = UserStateChange::new(
        ROOT,
        String::new(),
        FUNCTION,
        "other".to_owned(),
        SLOT,
        Some(1),
        RuntimeValue::BigInt(3),
        TEXT,
    )
    .expect("test change is valid");
    let first_key = first.key_without_principal().with_principal(PRINCIPAL);
    let second_key = second.key_without_principal().with_principal(PRINCIPAL);
    let mut current_cells = HashMap::from([
        (first_key, Some(current)),
        (second_key, Some(second_current)),
    ]);
    let error = plan_user_state_changes(&[first, second], PRINCIPAL, &mut current_cells)
        .expect_err("type mismatch aborts the batch");
    assert!(
        matches!(error, PostgresKernelError::UserState(_))
            && error.to_string().contains("ORNA0901")
    );
}
