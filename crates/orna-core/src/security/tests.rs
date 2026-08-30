use crate::system::{
    SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
    SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID, SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
};
use crate::{CatalogueRevisionId, SourceRevisionId};

use super::*;

const USER: PrincipalId = PrincipalId::from_bytes([1; 16]);
const FUNCTION: FunctionId = FunctionId::from_bytes([2; 16]);
const ROLE: PrincipalId = PrincipalId::from_bytes([5; 16]);
const OTHER_PRINCIPAL: PrincipalId = PrincipalId::from_bytes([6; 16]);
const OTHER_FUNCTION: FunctionId = FunctionId::from_bytes([7; 16]);
const REVISION: RevisionPair = RevisionPair::new(
    SourceRevisionId::from_bytes([3; 16]),
    CatalogueRevisionId::from_bytes([4; 16]),
);
const OTHER_REVISION: RevisionPair = RevisionPair::new(
    SourceRevisionId::from_bytes([8; 16]),
    CatalogueRevisionId::from_bytes([9; 16]),
);
const STD_FUNCTION: FunctionId = FunctionId::from_bytes([0x30; 16]);
const STD_REVISION: StandardLibraryRevisionId = StandardLibraryRevisionId::from_bytes([0x4e; 16]);
const OTHER_STANDARD_REVISION: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes([0x5e; 16]);
const STD_EXECUTABLE: FunctionRevisionId = FunctionRevisionId::from_bytes([0x31; 16]);
const OTHER_STD_EXECUTABLE: FunctionRevisionId = FunctionRevisionId::from_bytes([0x7b; 16]);

fn active(id: PrincipalId, kind: PrincipalKind) -> Principal {
    Principal::new(id, kind, PrincipalStatus::Active)
}

#[test]
fn direct_execute_grant_authorises_the_pinned_function() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![Principal::new(
            USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![ExecuteGrant::new(USER, FUNCTION)],
    )
    .expect("valid direct-grant snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");

    let ExecuteDecision::Allowed(evidence) =
        snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION))
    else {
        panic!("direct grant should allow execution");
    };

    assert_eq!(evidence.session_principal(), USER);
    assert_eq!(evidence.effective_principal(), USER);
    assert_eq!(evidence.active_roles(), &[]);
    assert_eq!(evidence.authorising_principal(), USER);
    assert_eq!(evidence.target(), InvocationTarget::new(FUNCTION, REVISION));
}

#[test]
fn security_context_digest_changes_when_validated_grant_set_changes() {
    let without_extra_grant = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION, OTHER_FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![ExecuteGrant::new(USER, FUNCTION)],
    )
    .expect("the base snapshot should be valid");
    let with_extra_grant = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION, OTHER_FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![
            ExecuteGrant::new(USER, FUNCTION),
            ExecuteGrant::new(USER, OTHER_FUNCTION),
        ],
    )
    .expect("the expanded grant snapshot should be valid");
    let first_session = without_extra_grant
        .bind_authenticated_session(USER, vec![])
        .expect("the base session should bind");
    let second_session = with_extra_grant
        .bind_authenticated_session(USER, vec![])
        .expect("the expanded session should bind");
    let target = InvocationTarget::new(FUNCTION, REVISION);

    let ExecuteDecision::Allowed(first_evidence) =
        without_extra_grant.authorise_execute(&first_session, target)
    else {
        panic!("the base grant should allow the target");
    };
    let ExecuteDecision::Allowed(second_evidence) =
        with_extra_grant.authorise_execute(&second_session, target)
    else {
        panic!("the common grant should allow the target");
    };

    assert_ne!(
        without_extra_grant.security_context_digest(),
        with_extra_grant.security_context_digest(),
        "the validated grant set must contribute to snapshot evidence",
    );
    assert_eq!(
        first_evidence.security_context_digest(),
        without_extra_grant.security_context_digest(),
    );
    assert_eq!(
        second_evidence.security_context_digest(),
        with_extra_grant.security_context_digest(),
    );
    assert_ne!(
        first_evidence.security_context_digest(),
        second_evidence.security_context_digest(),
        "same principal/session/target facts must not share changed grant evidence",
    );
}

#[test]
fn selected_reachable_role_grant_authorises_the_pinned_function() {
    let role = PrincipalId::from_bytes([5; 16]);
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![
            Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
            Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
        ],
        vec![RoleMembership::new(role, USER)],
        vec![ExecuteGrant::new(role, FUNCTION)],
    )
    .expect("valid role-grant snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![role])
        .expect("reachable active role should bind");

    let ExecuteDecision::Allowed(evidence) =
        snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION))
    else {
        panic!("selected role grant should allow execution");
    };

    assert_eq!(session.principal(), USER);
    assert_eq!(session.active_roles(), &[role]);
    assert_eq!(evidence.active_roles(), &[role]);
    assert_eq!(evidence.authorising_principal(), role);
}

#[test]
fn durable_class_wide_execute_grant_authorises_direct_session_principal() {
    let grant = PrivilegeGrant::new(USER, PrivilegeClass::Execute, None)
        .expect("a class-wide execute grant is valid");
    let snapshot =
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![SecurityFunctionTarget::application(FUNCTION)],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![],
            vec![],
            vec![grant],
        )
        .expect("valid durable direct-grant snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");

    let ExecuteDecision::Allowed(evidence) =
        snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION))
    else {
        panic!("durable direct grant should allow execution");
    };
    assert_eq!(evidence.authorising_principal(), USER);
}

#[test]
fn durable_object_scoped_execute_grant_authorises_selected_active_role() {
    let grant = PrivilegeGrant::new(ROLE, PrivilegeClass::Execute, Some(FUNCTION))
        .expect("an object-scoped execute grant is valid");
    let snapshot =
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![SecurityFunctionTarget::application(FUNCTION)],
            vec![
                active(USER, PrincipalKind::User),
                active(ROLE, PrincipalKind::Role),
            ],
            vec![RoleMembership::new(ROLE, USER)],
            vec![],
            vec![],
            vec![grant],
        )
        .expect("valid durable role-grant snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![ROLE])
        .expect("reachable active role should bind");

    let ExecuteDecision::Allowed(evidence) =
        snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION))
    else {
        panic!("durable role grant should allow execution");
    };
    assert_eq!(evidence.authorising_principal(), ROLE);
}

#[test]
fn durable_object_scoped_execute_grant_denies_an_unrelated_function() {
    let grant = PrivilegeGrant::new(USER, PrivilegeClass::Execute, Some(OTHER_FUNCTION))
        .expect("an object-scoped execute grant is valid");
    let snapshot =
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![
                SecurityFunctionTarget::application(FUNCTION),
                SecurityFunctionTarget::application(OTHER_FUNCTION),
            ],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![],
            vec![],
            vec![grant],
        )
        .expect("valid durable unrelated-object snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");

    assert_eq!(
        snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant)
    );
}

#[test]
fn authenticated_session_binding_is_clone_stable_distinct_and_redacted() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid session-binding snapshot");
    let first = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("first authenticated session should bind");
    let clone = first.clone();
    let second = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("second authenticated session should bind");

    assert_eq!(first.binding(), clone.binding());
    assert_ne!(first.binding(), second.binding());
    assert_eq!(
        format!("{:?}", first.binding()),
        "AuthenticatedSessionBinding(..)"
    );
}

#[test]
fn authenticated_session_equality_ignores_opaque_binding_identity() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![
            active(USER, PrincipalKind::User),
            active(ROLE, PrincipalKind::Role),
        ],
        vec![RoleMembership::new(ROLE, USER)],
        vec![],
    )
    .expect("valid session-binding snapshot");
    let first = snapshot
        .bind_authenticated_session(USER, vec![ROLE])
        .expect("first authenticated session should bind");
    let second = snapshot
        .bind_authenticated_session(USER, vec![ROLE])
        .expect("second authenticated session should bind");

    assert_ne!(first.binding(), second.binding());
    assert_eq!(first, second);
    assert_eq!(first.principal(), second.principal());
    assert_eq!(first.active_roles(), second.active_roles());
}

#[test]
fn unknown_principal_cannot_bind_an_authenticated_session() {
    let snapshot = SecuritySnapshot::new(REVISION, vec![FUNCTION], vec![], vec![], vec![])
        .expect("empty security catalogue is valid");

    assert_eq!(
        snapshot.bind_authenticated_session(USER, vec![]),
        Err(SessionBindingError::UnknownSessionPrincipal)
    );
}

#[test]
fn disabled_principal_cannot_bind_an_authenticated_session() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![Principal::new(
            USER,
            PrincipalKind::User,
            PrincipalStatus::Disabled,
        )],
        vec![],
        vec![],
    )
    .expect("disabled principals remain valid catalogue records");

    assert_eq!(
        snapshot.bind_authenticated_session(USER, vec![]),
        Err(SessionBindingError::DisabledSessionPrincipal)
    );
}

#[test]
fn role_cannot_bind_an_authenticated_session() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![Principal::new(
            USER,
            PrincipalKind::Role,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    )
    .expect("role principal is a valid catalogue record");

    assert_eq!(
        snapshot.bind_authenticated_session(USER, vec![]),
        Err(SessionBindingError::RoleCannotAuthenticate)
    );
}

#[test]
fn unreachable_role_cannot_be_selected_for_a_session() {
    let role = PrincipalId::from_bytes([5; 16]);
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![
            Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
            Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
        ],
        vec![],
        vec![ExecuteGrant::new(role, FUNCTION)],
    )
    .expect("ungranted role remains a valid catalogue record");

    assert_eq!(
        snapshot.bind_authenticated_session(USER, vec![role]),
        Err(SessionBindingError::UnreachableActiveRole)
    );
}

#[test]
fn duplicate_principal_rejects_the_complete_snapshot() {
    let principal = Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active);

    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![principal, principal],
            vec![],
            vec![],
        ),
        Err(SecuritySnapshotError::DuplicatePrincipal)
    ));
}

#[test]
fn malformed_functions_memberships_and_grants_reject_the_snapshot() {
    let user = active(USER, PrincipalKind::User);
    let role = active(ROLE, PrincipalKind::Role);

    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION, FUNCTION],
            vec![user],
            vec![],
            vec![],
        ),
        Err(SecuritySnapshotError::DuplicateFunction)
    ));
    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![user, role],
            vec![
                RoleMembership::new(ROLE, USER),
                RoleMembership::new(ROLE, USER),
            ],
            vec![],
        ),
        Err(SecuritySnapshotError::DuplicateMembership)
    ));
    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![role],
            vec![RoleMembership::new(ROLE, USER)],
            vec![],
        ),
        Err(SecuritySnapshotError::UnknownMembershipMember)
    ));
    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![user],
            vec![RoleMembership::new(ROLE, USER)],
            vec![],
        ),
        Err(SecuritySnapshotError::UnknownMembershipRole)
    ));
    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![user, active(OTHER_PRINCIPAL, PrincipalKind::User)],
            vec![RoleMembership::new(OTHER_PRINCIPAL, USER)],
            vec![],
        ),
        Err(SecuritySnapshotError::MembershipTargetIsNotRole)
    ));
    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![role],
            vec![RoleMembership::new(ROLE, ROLE)],
            vec![],
        ),
        Err(SecuritySnapshotError::SelfMembership)
    ));
    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![user],
            vec![],
            vec![ExecuteGrant::new(OTHER_PRINCIPAL, FUNCTION)],
        ),
        Err(SecuritySnapshotError::UnknownGrantPrincipal)
    ));
    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![user],
            vec![],
            vec![ExecuteGrant::new(USER, OTHER_FUNCTION)],
        ),
        Err(SecuritySnapshotError::UnknownGrantFunction)
    ));
    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![user],
            vec![],
            vec![
                ExecuteGrant::new(USER, FUNCTION),
                ExecuteGrant::new(USER, FUNCTION),
            ],
        ),
        Err(SecuritySnapshotError::DuplicateExecuteGrant)
    ));
}

#[test]
fn indirect_role_membership_cycle_rejects_the_snapshot() {
    assert!(matches!(
        SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![
                active(ROLE, PrincipalKind::Role),
                active(OTHER_PRINCIPAL, PrincipalKind::Role),
            ],
            vec![
                RoleMembership::new(ROLE, OTHER_PRINCIPAL),
                RoleMembership::new(OTHER_PRINCIPAL, ROLE),
            ],
            vec![],
        ),
        Err(SecuritySnapshotError::CyclicRoleMembership)
    ));
}

#[test]
fn active_role_selection_is_fail_closed_and_canonical() {
    let other_user = active(OTHER_PRINCIPAL, PrincipalKind::User);
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![
            active(USER, PrincipalKind::User),
            active(ROLE, PrincipalKind::Role),
        ],
        vec![RoleMembership::new(ROLE, USER)],
        vec![],
    )
    .expect("valid membership snapshot");

    assert_eq!(
        snapshot.bind_authenticated_session(USER, vec![ROLE, ROLE]),
        Err(SessionBindingError::DuplicateActiveRole)
    );
    assert_eq!(
        snapshot.bind_authenticated_session(USER, vec![OTHER_PRINCIPAL]),
        Err(SessionBindingError::UnknownActiveRole)
    );

    let disabled_snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![
            active(USER, PrincipalKind::User),
            Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Disabled),
        ],
        vec![RoleMembership::new(ROLE, USER)],
        vec![],
    )
    .expect("disabled role is valid catalogue state");
    assert_eq!(
        disabled_snapshot.bind_authenticated_session(USER, vec![ROLE]),
        Err(SessionBindingError::DisabledActiveRole)
    );

    let non_role_snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User), other_user],
        vec![],
        vec![],
    )
    .expect("two users form valid catalogue state");
    assert_eq!(
        non_role_snapshot.bind_authenticated_session(USER, vec![OTHER_PRINCIPAL]),
        Err(SessionBindingError::ActivePrincipalIsNotRole)
    );
}

#[test]
fn nested_selected_roles_are_ordered_and_unselected_roles_grant_nothing() {
    let outer_role = PrincipalId::from_bytes([10; 16]);
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![
            active(USER, PrincipalKind::Service),
            active(ROLE, PrincipalKind::Role),
            active(outer_role, PrincipalKind::Role),
        ],
        vec![
            RoleMembership::new(ROLE, USER),
            RoleMembership::new(outer_role, ROLE),
        ],
        vec![
            ExecuteGrant::new(ROLE, FUNCTION),
            ExecuteGrant::new(outer_role, FUNCTION),
        ],
    )
    .expect("valid nested-role snapshot");
    let no_roles = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("service session should bind");
    assert_eq!(
        snapshot.authorise_execute(&no_roles, InvocationTarget::new(FUNCTION, REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant)
    );

    let selected = snapshot
        .bind_authenticated_session(USER, vec![outer_role, ROLE])
        .expect("both reachable roles should bind");
    assert_eq!(selected.active_roles(), &[ROLE, outer_role]);
    let ExecuteDecision::Allowed(evidence) =
        snapshot.authorise_execute(&selected, InvocationTarget::new(FUNCTION, REVISION))
    else {
        panic!("selected nested roles should grant execution");
    };
    assert_eq!(evidence.authorising_principal(), ROLE);
}

#[test]
fn disabled_intermediary_role_does_not_reach_nested_active_role() {
    let role_a = ROLE;
    let role_b = OTHER_PRINCIPAL;
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![
            active(USER, PrincipalKind::User),
            Principal::new(role_a, PrincipalKind::Role, PrincipalStatus::Disabled),
            active(role_b, PrincipalKind::Role),
        ],
        vec![
            RoleMembership::new(role_a, USER),
            RoleMembership::new(role_b, role_a),
        ],
        vec![ExecuteGrant::new(role_b, FUNCTION)],
    )
    .expect("disabled intermediary role remains valid catalogue state");

    assert_eq!(
        snapshot.bind_authenticated_session(USER, vec![role_b]),
        Err(SessionBindingError::UnreachableActiveRole)
    );
}

#[test]
fn wrong_function_revision_and_missing_grant_are_typed_denials() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION, OTHER_FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![ExecuteGrant::new(USER, FUNCTION)],
    )
    .expect("valid direct-grant snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");

    assert_eq!(
        snapshot.authorise_execute(
            &session,
            InvocationTarget::new(FunctionId::from_bytes([11; 16]), REVISION),
        ),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
    );
    assert_eq!(
        snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, OTHER_REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::RevisionMismatch)
    );
    assert_eq!(
        snapshot.authorise_execute(&session, InvocationTarget::new(OTHER_FUNCTION, REVISION),),
        ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant)
    );
}

#[test]
fn catalogue_health_has_one_stable_system_identity_and_name() {
    assert_eq!(
        CATALOGUE_HEALTH_FUNCTION_ID.to_bytes(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
    assert_eq!(CATALOGUE_HEALTH_FUNCTION_NAME, "sys.catalog.health");
    assert_eq!(
        CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.to_bytes(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
}

#[test]
fn authenticated_catalogue_health_needs_no_application_grant() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::Service)],
        vec![],
        vec![],
    )
    .expect("valid service snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active service session should bind");
    let target = InvocationTarget::new(CATALOGUE_HEALTH_FUNCTION_ID, REVISION);

    let ExecuteDecision::Allowed(evidence) = snapshot.authorise_catalogue_health(&session, target)
    else {
        panic!("authenticated catalogue health should be allowed");
    };
    assert_eq!(evidence.session_principal(), USER);
    assert_eq!(evidence.effective_principal(), USER);
    assert_eq!(evidence.authorising_principal(), USER);
    assert_eq!(evidence.active_roles(), &[]);
    assert_eq!(evidence.target(), target);

    assert_eq!(
        snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant)
    );
}

#[test]
fn sealed_system_target_accepts_classless_unpinned_shape() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid authenticated snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active session should bind");
    let target = InvocationTarget::new(crate::system::SYS_INVOKE_FUNCTION_ID, REVISION);

    assert_eq!(target.class(), None);
    assert_eq!(target.standard_revision(), None);
    assert_eq!(target.executable_revision(), None);
    assert!(matches!(
        snapshot.authorise_system_function(&session, target),
        ExecuteDecision::Allowed(_)
    ));
}

#[test]
fn sealed_system_target_rejects_forged_class_and_revision_pins() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid authenticated snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active session should bind");
    let forged = InvocationTarget {
        function: crate::system::SYS_INVOKE_FUNCTION_ID,
        revision: REVISION,
        class: Some(TargetClass::Application),
        standard_revision: Some(OTHER_STANDARD_REVISION),
        executable_revision: Some(OTHER_STD_EXECUTABLE),
    };

    assert_eq!(
        snapshot.authorise_system_function(&session, forged),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
    );
}

#[test]
fn authenticated_user_and_service_enter_every_registered_system_function_without_a_grant() {
    // The system entry decision must not depend on a stored application
    // grant and must not consult the application function set, which
    // deliberately excludes the sealed identities.
    for kind in [PrincipalKind::User, PrincipalKind::Service] {
        let snapshot = SecuritySnapshot::new(
            REVISION,
            vec![FUNCTION],
            vec![active(USER, kind)],
            vec![],
            vec![],
        )
        .expect("valid authenticated snapshot");
        let session = snapshot
            .bind_authenticated_session(USER, vec![])
            .expect("active session should bind");

        for system_function in crate::system::SYSTEM_FUNCTIONS {
            let target = InvocationTarget::new(system_function.id(), REVISION);
            let ExecuteDecision::Allowed(evidence) =
                snapshot.authorise_system_function(&session, target)
            else {
                panic!("a registered system function must be authorised without a grant");
            };
            assert_eq!(evidence.session_principal(), USER);
            assert_eq!(evidence.effective_principal(), USER);
            assert_eq!(evidence.authorising_principal(), USER);
            assert_eq!(evidence.active_roles(), &[]);
            assert_eq!(evidence.target(), target);
        }

        assert_eq!(
            snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION)),
            ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant)
        );
    }
}

#[test]
fn system_entry_decision_preserves_exact_denials_and_precedence() {
    let enabled = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![ExecuteGrant::new(USER, FUNCTION)],
    )
    .expect("granted snapshot should validate");
    let session = enabled
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");
    let invoke = crate::system::SYS_INVOKE_FUNCTION_ID;

    // A stale revision is rejected before the registry lookup.
    assert_eq!(
        enabled.authorise_system_function(&session, InvocationTarget::new(invoke, OTHER_REVISION),),
        ExecuteDecision::Denied(ExecuteDenial::RevisionMismatch)
    );
    // An identity outside the registry and the application set is unknown.
    assert_eq!(
        enabled
            .authorise_system_function(&session, InvocationTarget::new(OTHER_FUNCTION, REVISION),),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
    );
    // The direct grant opens ordinary execution but never the closed
    // system entry, which admits only exact registered system functions.
    assert!(
        matches!(
            enabled.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION)),
            ExecuteDecision::Allowed(_)
        ),
        "the direct grant must authorise ordinary application execution"
    );
    assert_eq!(
        enabled.authorise_system_function(&session, InvocationTarget::new(FUNCTION, REVISION),),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
    );
    // A session is revalidated against the deciding snapshot, so the
    // disabled-principal snapshot denies the same session state.
    let disabled = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![Principal::new(
            USER,
            PrincipalKind::User,
            PrincipalStatus::Disabled,
        )],
        vec![],
        vec![],
    )
    .expect("disabled principal remains durable snapshot state");
    assert_eq!(
        disabled.authorise_system_function(&session, InvocationTarget::new(invoke, REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::InvalidSession)
    );
    // The retained compatibility method stays health-only and never
    // admits the registered invocation gateway.
    assert_eq!(
        enabled.authorise_catalogue_health(&session, InvocationTarget::new(invoke, REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
    );
}

#[test]
fn authorise_system_function_rejects_hostile_session_state() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![
            active(USER, PrincipalKind::User),
            active(ROLE, PrincipalKind::Role),
        ],
        vec![],
        vec![],
    )
    .expect("hostile-state snapshot should validate");
    let target = InvocationTarget::new(crate::system::SYS_INVOKE_FUNCTION_ID, REVISION);

    // The public binder already rejects the role and the unreachable
    // role, so the denial inside authorise_system_function is the same
    // session validation and not a separate rule.
    assert_eq!(
        snapshot.bind_authenticated_session(ROLE, vec![]),
        Err(SessionBindingError::RoleCannotAuthenticate)
    );
    assert_eq!(
        snapshot.bind_authenticated_session(USER, vec![ROLE]),
        Err(SessionBindingError::UnreachableActiveRole)
    );

    // Sessions are constructed inside the module boundary to prove that
    // authorisation never trusts pre-bound session state.
    let unknown_principal = AuthenticatedSession {
        principal: OTHER_PRINCIPAL,
        active_roles: vec![],
        binding: AuthenticatedSessionBinding::new(),
    };
    let role_pretending = AuthenticatedSession {
        principal: ROLE,
        active_roles: vec![],
        binding: AuthenticatedSessionBinding::new(),
    };
    let user_with_unreachable_role = AuthenticatedSession {
        principal: USER,
        active_roles: vec![ROLE],
        binding: AuthenticatedSessionBinding::new(),
    };

    for hostile in [
        &unknown_principal,
        &role_pretending,
        &user_with_unreachable_role,
    ] {
        assert_eq!(
            snapshot.authorise_system_function(hostile, target),
            ExecuteDecision::Denied(ExecuteDenial::InvalidSession)
        );
    }
}

#[test]
fn catalogue_health_rejects_every_other_target_or_session() {
    let enabled = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid user snapshot");
    let session = enabled
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");
    assert_eq!(
        enabled.authorise_catalogue_health(
            &session,
            InvocationTarget::new(CATALOGUE_HEALTH_FUNCTION_ID, OTHER_REVISION),
        ),
        ExecuteDecision::Denied(ExecuteDenial::RevisionMismatch)
    );
    let application_target = InvocationTarget::new(FUNCTION, REVISION);
    assert_eq!(
        enabled.authorise_catalogue_health(&session, application_target),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
    );

    let disabled = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![Principal::new(
            USER,
            PrincipalKind::User,
            PrincipalStatus::Disabled,
        )],
        vec![],
        vec![],
    )
    .expect("disabled principal remains valid catalogue state");
    assert_eq!(
        disabled.authorise_catalogue_health(
            &session,
            InvocationTarget::new(CATALOGUE_HEALTH_FUNCTION_ID, REVISION),
        ),
        ExecuteDecision::Denied(ExecuteDenial::InvalidSession)
    );
}

#[test]
fn session_from_another_snapshot_is_revalidated_before_authorisation() {
    let first = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![ExecuteGrant::new(USER, FUNCTION)],
    )
    .expect("first snapshot should validate");
    let session = first
        .bind_authenticated_session(USER, vec![])
        .expect("first snapshot should bind the session");
    let disabled = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![Principal::new(
            USER,
            PrincipalKind::User,
            PrincipalStatus::Disabled,
        )],
        vec![],
        vec![ExecuteGrant::new(USER, FUNCTION)],
    )
    .expect("disabled principal remains durable state");

    assert_eq!(
        disabled.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::InvalidSession)
    );
}

#[test]
fn snapshot_exposes_canonical_persistence_records() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![OTHER_FUNCTION, FUNCTION],
        vec![
            active(ROLE, PrincipalKind::Role),
            active(USER, PrincipalKind::User),
        ],
        vec![RoleMembership::new(ROLE, USER)],
        vec![
            ExecuteGrant::new(ROLE, OTHER_FUNCTION),
            ExecuteGrant::new(USER, FUNCTION),
        ],
    )
    .expect("valid persistence snapshot");

    assert_eq!(snapshot.revision(), REVISION);
    assert_eq!(
        snapshot.functions().collect::<Vec<_>>(),
        vec![FUNCTION, OTHER_FUNCTION]
    );
    assert_eq!(
        snapshot.principals().map(Principal::id).collect::<Vec<_>>(),
        vec![USER, ROLE]
    );
    assert_eq!(
        snapshot.memberships().collect::<Vec<_>>(),
        vec![RoleMembership::new(ROLE, USER)]
    );
    assert_eq!(
        snapshot.execute_grants().collect::<Vec<_>>(),
        vec![
            ExecuteGrant::new(USER, FUNCTION),
            ExecuteGrant::new(ROLE, OTHER_FUNCTION),
        ]
    );
}

#[test]
fn local_peer_authentication_binds_only_the_mapped_principal() {
    let snapshot = SecuritySnapshot::new_with_local_peer_credentials(
        REVISION,
        vec![FUNCTION],
        vec![
            active(USER, PrincipalKind::User),
            active(ROLE, PrincipalKind::Role),
        ],
        vec![RoleMembership::new(ROLE, USER)],
        vec![ExecuteGrant::new(ROLE, FUNCTION)],
        vec![LocalPeerCredential::new(1_001, USER)],
    )
    .expect("valid local peer snapshot");

    let session = snapshot
        .authenticate_local_peer(1_001)
        .expect("mapped active user should authenticate");

    assert_eq!(session.principal(), USER);
    assert_eq!(session.active_roles(), &[]);
    assert_eq!(
        snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant)
    );
    assert_eq!(
        snapshot.authenticate_local_peer(1_002),
        Err(LocalPeerAuthenticationError::UnknownUid)
    );
    assert_eq!(
        snapshot.local_peer_credentials().collect::<Vec<_>>(),
        vec![LocalPeerCredential::new(1_001, USER)]
    );
}

#[test]
fn local_peer_authentication_rejects_disabled_and_role_principals() {
    for (principal, expected) in [
        (
            Principal::new(USER, PrincipalKind::User, PrincipalStatus::Disabled),
            SessionBindingError::DisabledSessionPrincipal,
        ),
        (
            active(USER, PrincipalKind::Role),
            SessionBindingError::RoleCannotAuthenticate,
        ),
    ] {
        let snapshot = SecuritySnapshot::new_with_local_peer_credentials(
            REVISION,
            vec![FUNCTION],
            vec![principal],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(1_001, USER)],
        )
        .expect("existing principal may retain a local credential");

        assert_eq!(
            snapshot.authenticate_local_peer(1_001),
            Err(LocalPeerAuthenticationError::InvalidPrincipal(expected))
        );
    }
}

#[test]
fn malformed_local_peer_credentials_reject_the_snapshot() {
    let credential = LocalPeerCredential::new(1_001, USER);
    let other_uid = LocalPeerCredential::new(1_002, USER);
    let other_principal = LocalPeerCredential::new(1_001, OTHER_PRINCIPAL);
    let principals = vec![
        active(USER, PrincipalKind::User),
        active(OTHER_PRINCIPAL, PrincipalKind::Service),
    ];

    assert!(matches!(
        SecuritySnapshot::new_with_local_peer_credentials(
            REVISION,
            vec![FUNCTION],
            principals.clone(),
            vec![],
            vec![],
            vec![credential, other_principal],
        ),
        Err(SecuritySnapshotError::DuplicateLocalPeerUid)
    ));
    assert!(matches!(
        SecuritySnapshot::new_with_local_peer_credentials(
            REVISION,
            vec![FUNCTION],
            principals.clone(),
            vec![],
            vec![],
            vec![credential, other_uid],
        ),
        Err(SecuritySnapshotError::DuplicateLocalPeerPrincipal)
    ));
    assert!(matches!(
        SecuritySnapshot::new_with_local_peer_credentials(
            REVISION,
            vec![FUNCTION],
            principals,
            vec![],
            vec![],
            vec![LocalPeerCredential::new(
                1_003,
                PrincipalId::from_bytes([0xff; 16]),
            )],
        ),
        Err(SecuritySnapshotError::UnknownLocalPeerPrincipal)
    ));
}

#[test]
fn security_audit_decisions_expose_only_closed_decision_evidence() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![ExecuteGrant::new(USER, FUNCTION)],
    )
    .expect("valid audit decision snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("valid audit session");
    let target = InvocationTarget::new(FUNCTION, REVISION);
    let ExecuteDecision::Allowed(authorised) = snapshot.authorise_execute(&session, target) else {
        panic!("direct grant should create allowed evidence");
    };

    let authenticated = SecurityAuditDecision::authentication_allowed(&session);
    assert_eq!(authenticated.kind(), SecurityAuditKind::Authentication);
    assert_eq!(authenticated.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(authenticated.session_principal(), Some(USER));
    assert_eq!(authenticated.effective_principal(), None);
    assert_eq!(authenticated.authorising_principal(), None);
    assert_eq!(authenticated.target(), None);
    assert_eq!(authenticated.denial(), None);

    let allowed = SecurityAuditDecision::execute_allowed(&authorised);
    assert_eq!(allowed.kind(), SecurityAuditKind::Execute);
    assert_eq!(allowed.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(allowed.session_principal(), Some(USER));
    assert_eq!(allowed.effective_principal(), Some(USER));
    assert_eq!(allowed.authorising_principal(), Some(USER));
    assert_eq!(allowed.target(), Some(target));
    assert_eq!(allowed.denial(), None);

    let denied =
        SecurityAuditDecision::execute_denied(&session, target, ExecuteDenial::MissingExecuteGrant);
    assert_eq!(denied.kind(), SecurityAuditKind::Execute);
    assert_eq!(denied.outcome(), SecurityAuditOutcome::Denied);
    assert_eq!(denied.session_principal(), Some(USER));
    assert_eq!(denied.effective_principal(), None);
    assert_eq!(denied.authorising_principal(), None);
    assert_eq!(denied.target(), Some(target));
    assert_eq!(
        denied.denial(),
        Some(SecurityAuditDenial::Execute(
            ExecuteDenial::MissingExecuteGrant
        ))
    );
}
#[test]
fn user_state_audit_decision_retains_only_redacted_operation_facts() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid USER state audit snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("valid USER state audit session");
    let decision = SecurityAuditDecision::user_state_allowed(
        &session,
        UserStateAuditOperation::Write,
        FUNCTION,
        7,
    );
    assert_eq!(decision.kind(), SecurityAuditKind::UserState);
    assert_eq!(decision.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(decision.session_principal(), Some(USER));
    assert_eq!(
        decision.user_state_operation(),
        Some(UserStateAuditOperation::Write)
    );
    assert_eq!(decision.user_state_root_function(), Some(FUNCTION));
    assert_eq!(decision.user_state_cell_count(), Some(7));
    assert_eq!(decision.effective_principal(), None);
    assert_eq!(decision.authorising_principal(), None);
    assert_eq!(decision.target(), None);
    assert_eq!(decision.denial(), None);
}

#[test]
fn authentication_audit_denials_require_exact_principal_shape() {
    let unknown = SecurityAuditDecision::authentication_denied(
        None,
        LocalPeerAuthenticationError::UnknownUid,
    )
    .expect("unknown UID has no principal evidence");
    assert_eq!(unknown.session_principal(), None);
    assert_eq!(unknown.outcome(), SecurityAuditOutcome::Denied);
    assert_eq!(
        unknown.denial(),
        Some(SecurityAuditDenial::Authentication(
            LocalPeerAuthenticationError::UnknownUid
        ))
    );

    let invalid = SecurityAuditDecision::authentication_denied(
        Some(USER),
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::DisabledSessionPrincipal,
        ),
    )
    .expect("invalid mapped principal is retained");
    assert_eq!(invalid.session_principal(), Some(USER));
    assert_eq!(invalid.kind(), SecurityAuditKind::Authentication);

    assert_eq!(
        SecurityAuditDecision::authentication_denied(
            Some(USER),
            LocalPeerAuthenticationError::UnknownUid,
        ),
        Err(SecurityAuditDecisionError::AuthenticationPrincipalShape)
    );
    assert_eq!(
        SecurityAuditDecision::authentication_denied(
            None,
            LocalPeerAuthenticationError::InvalidPrincipal(
                SessionBindingError::RoleCannotAuthenticate,
            ),
        ),
        Err(SecurityAuditDecisionError::AuthenticationPrincipalShape)
    );
}

#[test]
fn capability_audit_decisions_record_redacted_qualified_names() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![ExecuteGrant::new(USER, FUNCTION)],
    )
    .expect("valid capability audit decision snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("valid capability audit session");
    let target = InvocationTarget::new(FUNCTION, REVISION);

    let allowed = SecurityAuditDecision::capability_allowed(&session, target, "std.fs.read")
        .expect("closed capability name is valid");
    assert_eq!(allowed.kind(), SecurityAuditKind::Capability);
    assert_eq!(allowed.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(allowed.session_principal(), Some(USER));
    assert_eq!(allowed.effective_principal(), None);
    assert_eq!(allowed.authorising_principal(), None);
    assert_eq!(allowed.target(), Some(target));
    assert_eq!(allowed.capability_name(), Some("std.fs.read"));
    assert_eq!(allowed.denial(), None);

    let denied =
        SecurityAuditDecision::capability_denied(&session, target, "std.net.connect".to_owned())
            .expect("closed capability name is valid");
    assert_eq!(denied.kind(), SecurityAuditKind::Capability);
    assert_eq!(denied.outcome(), SecurityAuditOutcome::Denied);
    assert_eq!(denied.session_principal(), Some(USER));
    assert_eq!(denied.effective_principal(), None);
    assert_eq!(denied.authorising_principal(), None);
    assert_eq!(denied.target(), Some(target));
    assert_eq!(denied.capability_name(), Some("std.net.connect"));
    assert_eq!(
        denied.denial(),
        Some(SecurityAuditDenial::Capability {
            capability: "std.net.connect".to_owned(),
        })
    );
}

#[test]
fn capability_audit_names_are_closed_qualified_names_without_arguments() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![ExecuteGrant::new(USER, FUNCTION)],
    )
    .expect("valid capability name shape snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("valid capability name shape session");
    let target = InvocationTarget::new(FUNCTION, REVISION);

    for name in [
        "std.fs.read",
        "std.fs.write",
        "std.net.connect",
        "std.secret.use",
        "std.fs.read_2",
        "std.v1.value",
    ] {
        assert_eq!(
            SecurityAuditDecision::capability_allowed(&session, target, name)
                .expect("qualified capability name must record")
                .capability_name(),
            Some(name),
            "{name:?} is a closed qualified capability name"
        );
    }

    for name in [
        "",
        "read",
        "std.fs.read(p)",
        "std.fs.read(/home/bob)",
        "/home/bob",
        "std.secret.my-secret",
        "std.fs.READ",
        "std.fs.read.",
        "std..read",
        "std fs.read",
        "Std.fs.read",
        "std.fs.1read",
    ] {
        assert_eq!(
            SecurityAuditDecision::capability_allowed(&session, target, name),
            Err(SecurityAuditDecisionError::CapabilityNameShape),
            "{name:?} must be rejected as an unredacted capability name"
        );
    }
}

#[test]
fn audit_events_preserve_exact_signed_order_and_recording_time() {
    use std::time::{Duration, UNIX_EPOCH};

    let decision = SecurityAuditDecision::authentication_denied(
        None,
        LocalPeerAuthenticationError::UnknownUid,
    )
    .expect("valid unknown-peer audit decision");
    let id = SecurityAuditEventId::from_bytes([0x42; 16]);
    let recorded_at = UNIX_EPOCH - Duration::from_secs(1);
    let event = SecurityAuditEvent::new(id, -7, recorded_at, decision.clone());

    assert_eq!(event.id(), id);
    assert_eq!(event.sequence(), -7);
    assert_eq!(event.recorded_at(), recorded_at);
    assert_eq!(event.decision(), &decision);
}

#[test]
fn two_class_union_admits_standard_grants_only_from_the_pinned_snapshot() {
    let standard_target =
        SecurityFunctionTarget::verified_standard(STD_FUNCTION, STD_REVISION, STD_EXECUTABLE);

    let snapshot = SecuritySnapshot::new_with_function_targets(
        REVISION,
        vec![standard_target],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![ExecuteGrant::new(USER, STD_FUNCTION)],
    )
    .expect("a grant naming a function in the pinned standard snapshot must be admitted");
    assert!(
        snapshot
            .function_targets()
            .any(|target| target == standard_target)
    );

    assert!(matches!(
        SecuritySnapshot::new_with_function_targets(
            REVISION,
            vec![],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![ExecuteGrant::new(USER, STD_FUNCTION)],
        ),
        Err(SecuritySnapshotError::UnknownGrantFunction)
    ));

    assert!(matches!(
        SecuritySnapshot::new_with_function_targets(
            REVISION,
            vec![
                SecurityFunctionTarget::verified_standard(
                    STD_FUNCTION,
                    STD_REVISION,
                    STD_EXECUTABLE,
                ),
                SecurityFunctionTarget::verified_standard(
                    STD_FUNCTION,
                    STD_REVISION,
                    STD_EXECUTABLE,
                ),
            ],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![],
        ),
        Err(SecuritySnapshotError::DuplicateFunction)
    ));

    assert!(matches!(
        SecuritySnapshot::new_with_function_targets(
            REVISION,
            vec![
                SecurityFunctionTarget::application(STD_FUNCTION),
                SecurityFunctionTarget::verified_standard(
                    STD_FUNCTION,
                    STD_REVISION,
                    STD_EXECUTABLE,
                ),
            ],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![],
        ),
        Err(SecuritySnapshotError::DuplicateFunction)
    ));

    assert_eq!(snapshot.functions().collect::<Vec<_>>(), [STD_FUNCTION]);
}

#[test]
fn authorise_execute_enforces_class_and_immutable_standard_pins() {
    let snapshot = SecuritySnapshot::new_with_function_targets(
        REVISION,
        vec![
            SecurityFunctionTarget::application(FUNCTION),
            SecurityFunctionTarget::verified_standard(STD_FUNCTION, STD_REVISION, STD_EXECUTABLE),
        ],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![
            ExecuteGrant::new(USER, FUNCTION),
            ExecuteGrant::new(USER, STD_FUNCTION),
        ],
    )
    .expect("a two-class snapshot with grants");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("an active user session");

    assert_eq!(
        snapshot.functions().collect::<Vec<_>>(),
        [FUNCTION, STD_FUNCTION],
        "the two-class union is canonical and identity-ordered"
    );
    assert_eq!(
        snapshot.function_targets().collect::<Vec<_>>(),
        [
            SecurityFunctionTarget::application(FUNCTION),
            SecurityFunctionTarget::verified_standard(STD_FUNCTION, STD_REVISION, STD_EXECUTABLE,),
        ]
    );

    assert!(matches!(
        snapshot.authorise_execute(
            &session,
            InvocationTarget::verified_standard(
                STD_FUNCTION,
                REVISION,
                STD_REVISION,
                STD_EXECUTABLE,
            ),
        ),
        ExecuteDecision::Allowed(_)
    ));
    assert!(matches!(
        snapshot.authorise_execute(&session, InvocationTarget::new(FUNCTION, REVISION)),
        ExecuteDecision::Allowed(_)
    ));

    assert_eq!(
        snapshot.authorise_execute(&session, InvocationTarget::new(STD_FUNCTION, REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction),
        "a class-less raw target never authorises a verified-standard function"
    );
    assert_eq!(
        snapshot.authorise_execute(
            &session,
            InvocationTarget::verified_standard(FUNCTION, REVISION, STD_REVISION, STD_EXECUTABLE,),
        ),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction),
        "a verified-standard claim never authorises an application function"
    );
    assert_eq!(
        snapshot.authorise_execute(
            &session,
            InvocationTarget::verified_standard(
                STD_FUNCTION,
                REVISION,
                STD_REVISION,
                OTHER_STD_EXECUTABLE,
            ),
        ),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction),
        "a wrong executable pin must be denied closed"
    );
    assert_eq!(
        snapshot.authorise_execute(
            &session,
            InvocationTarget::verified_standard(
                STD_FUNCTION,
                REVISION,
                OTHER_STANDARD_REVISION,
                STD_EXECUTABLE,
            ),
        ),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction),
        "a wrong standard snapshot pin must be denied closed"
    );
    assert_eq!(
        snapshot.authorise_execute(&session, InvocationTarget::new(OTHER_FUNCTION, REVISION)),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction),
        "a function absent from the union is unknown"
    );
}

#[test]
fn flat_constructor_stays_application_only_and_rejects_standard_class_claims() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![STD_FUNCTION, FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("the flat application constructor remains valid");
    assert_eq!(
        snapshot.functions().collect::<Vec<_>>(),
        [FUNCTION, STD_FUNCTION],
        "the flat function set is canonical and identity-ordered"
    );
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("an active user session");
    assert_eq!(
        snapshot.authorise_execute(
            &session,
            InvocationTarget::verified_standard(
                STD_FUNCTION,
                REVISION,
                STD_REVISION,
                STD_EXECUTABLE,
            ),
        ),
        ExecuteDecision::Denied(ExecuteDenial::UnknownFunction),
        "the flat constructor admits only Application targets"
    );
}

#[test]
fn inspect_own_grant_reaches_only_own_epochs() {
    assert_eq!(
        authorise_inspect(
            USER,
            InspectPrivilege::OwnInvocation,
            Some(USER),
            &[InspectPrivilege::OwnInvocation],
        ),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Own,
            requested: InspectPrivilege::OwnInvocation,
        }
    );
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::OwnInvocation,
            None,
            &[InspectPrivilege::OwnInvocation],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::OwnInvocation,
            Some(OTHER_PRINCIPAL),
            &[InspectPrivilege::OwnInvocation],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
}

#[test]
fn inspect_session_grant_reaches_own_and_session_epochs() {
    let granted = [InspectPrivilege::SessionInvocations];
    assert_eq!(
        authorise_inspect(
            USER,
            InspectPrivilege::SessionInvocations,
            Some(USER),
            &granted
        ),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Own,
            requested: InspectPrivilege::SessionInvocations,
        }
    );
    assert_eq!(
        authorise_inspect(USER, InspectPrivilege::SessionInvocations, None, &granted),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Session,
            requested: InspectPrivilege::SessionInvocations,
        }
    );
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::SessionInvocations,
            Some(OTHER_PRINCIPAL),
            &granted
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::OwnInvocation,
            None,
            &[InspectPrivilege::OwnInvocation]
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
}

#[test]
fn inspect_any_grant_reaches_every_epoch() {
    let granted = [InspectPrivilege::AnyInvocation];
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::AnyInvocation,
            Some(OTHER_PRINCIPAL),
            &granted
        ),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Foreign,
            ..
        }
    ));
    assert!(matches!(
        authorise_inspect(USER, InspectPrivilege::AnyInvocation, Some(USER), &granted),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Own,
            ..
        }
    ));
    assert!(matches!(
        authorise_inspect(USER, InspectPrivilege::AnyInvocation, None, &granted),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Session,
            ..
        }
    ));
}

#[test]
fn inspect_classifiers_are_orthogonal_and_independent() {
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::Values,
            Some(USER),
            &[InspectPrivilege::OwnInvocation, InspectPrivilege::Source],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::OwnInvocation,
            Some(USER),
            &[InspectPrivilege::Values],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::Values,
            Some(USER),
            &[InspectPrivilege::OwnInvocation, InspectPrivilege::Values],
        ),
        InspectDecision::Allowed {
            epoch_scope: InspectEpochScope::Own,
            requested: InspectPrivilege::Values,
        }
    ));
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::RuntimeInternals,
            Some(OTHER_PRINCIPAL),
            &[
                InspectPrivilege::OwnInvocation,
                InspectPrivilege::RuntimeInternals,
            ],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
}

#[test]
fn inspect_ladder_denies_when_the_requested_rung_is_not_granted() {
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::SessionInvocations,
            Some(USER),
            &[InspectPrivilege::OwnInvocation],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
    assert!(matches!(
        authorise_inspect(
            USER,
            InspectPrivilege::AnyInvocation,
            Some(USER),
            &[InspectPrivilege::SessionInvocations],
        ),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
    assert!(matches!(
        authorise_inspect(USER, InspectPrivilege::OwnInvocation, Some(USER), &[]),
        InspectDecision::Denied(InspectDenial::MissingPrivilege)
    ));
}

#[test]
fn inspect_denials_carry_closed_audit_reasons() {
    assert_eq!(
        InspectDenial::MissingPrivilege.audit_reason(),
        "inspect:missing-privilege"
    );
    assert_eq!(
        InspectDenial::MissingEpoch.audit_reason(),
        "inspect:missing-epoch"
    );
    assert_eq!(
        InspectDenial::ObserverSuppressed.audit_reason(),
        "inspect:observer-suppressed"
    );
}

#[test]
fn inspect_audit_decisions_record_epoch_access_facts() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid INSPECT audit session");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");

    let decision = authorise_inspect(
        USER,
        InspectPrivilege::OwnInvocation,
        Some(USER),
        &[InspectPrivilege::OwnInvocation],
    );
    let allowed = SecurityAuditDecision::inspect_allowed(&session, decision, Some(USER))
        .expect("an allowed decision must record");
    assert_eq!(allowed.kind(), SecurityAuditKind::Inspect);
    assert_eq!(allowed.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(allowed.session_principal(), Some(USER));
    assert_eq!(
        allowed.inspect_requested(),
        Some(InspectPrivilege::OwnInvocation)
    );
    assert_eq!(allowed.inspect_epoch_scope(), Some(InspectEpochScope::Own));
    assert_eq!(allowed.denial(), None);

    let recovered = SecurityAuditDecision::recover_inspect_allowed(
        USER,
        InspectPrivilege::Values,
        InspectEpochScope::Foreign,
        Some(OTHER_PRINCIPAL),
    );
    assert_eq!(recovered.kind(), SecurityAuditKind::Inspect);
    assert_eq!(
        recovered.inspect_requested(),
        Some(InspectPrivilege::Values)
    );
    assert_eq!(
        recovered.inspect_epoch_scope(),
        Some(InspectEpochScope::Foreign)
    );

    let denied = SecurityAuditDecision::inspect_denied(
        &session,
        Some(OTHER_PRINCIPAL),
        InspectDenial::MissingPrivilege,
    );
    assert_eq!(denied.kind(), SecurityAuditKind::Inspect);
    assert_eq!(denied.outcome(), SecurityAuditOutcome::Denied);
    assert_eq!(denied.session_principal(), Some(USER));
    assert_eq!(
        denied.denial(),
        Some(SecurityAuditDenial::Inspect(
            InspectDenial::MissingPrivilege
        ))
    );
    assert_eq!(
        denied.inspect_denial(),
        Some(InspectDenial::MissingPrivilege)
    );
    assert_eq!(denied.inspect_requested(), None);
}

#[test]
fn inspect_audit_allowed_rejects_a_denied_decision_shape() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid INSPECT audit session");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");
    assert_eq!(
        SecurityAuditDecision::inspect_allowed(
            &session,
            InspectDecision::Denied(InspectDenial::MissingPrivilege),
            Some(USER),
        ),
        Err(SecurityAuditDecisionError::InspectDecisionShape)
    );
}

#[test]
fn privilege_class_display_uses_closed_canonical_strings() {
    assert_eq!(PrivilegeClass::Execute.to_string(), "execute");
    assert_eq!(PrivilegeClass::SecurityAdmin.to_string(), "security_admin");
    assert_eq!(
        PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation).to_string(),
        "inspect:own-invocation"
    );
    assert_eq!(
        PrivilegeClass::Inspect(InspectPrivilege::SessionInvocations).to_string(),
        "inspect:session-invocations"
    );
    assert_eq!(
        PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation).to_string(),
        "inspect:any-invocation"
    );
    assert_eq!(
        PrivilegeClass::Inspect(InspectPrivilege::Values).to_string(),
        "inspect:values"
    );
    assert_eq!(
        PrivilegeClass::Inspect(InspectPrivilege::Source).to_string(),
        "inspect:source"
    );
    assert_eq!(
        PrivilegeClass::Inspect(InspectPrivilege::SecurityDetails).to_string(),
        "inspect:security-details"
    );
    assert_eq!(
        PrivilegeClass::Inspect(InspectPrivilege::RuntimeInternals).to_string(),
        "inspect:runtime-internals"
    );
}

#[test]
fn principal_checked_constructor_rejects_empty_identity() {
    assert!(matches!(
        Principal::try_new(
            PrincipalId::from_bytes([0; 16]),
            PrincipalKind::User,
            PrincipalStatus::Active,
        ),
        Err(SecuritySnapshotError::EmptyPrincipal)
    ));
    assert_eq!(
        Principal::try_new(USER, PrincipalKind::User, PrincipalStatus::Active)
            .expect("non-empty principal should construct")
            .id(),
        USER
    );
}

#[test]
fn security_snapshot_rejects_empty_principal_before_authorisation() {
    let result = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![Principal::new(
            PrincipalId::from_bytes([0; 16]),
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    );
    assert!(matches!(result, Err(SecuritySnapshotError::EmptyPrincipal)));
}

#[test]
fn zero_principal_cannot_authenticate_or_authorise() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid security snapshot");
    assert_eq!(
        snapshot.bind_authenticated_session(PrincipalId::from_bytes([0; 16]), vec![]),
        Err(SessionBindingError::UnknownSessionPrincipal)
    );
}

#[test]
fn privilege_grant_constructor_rejects_empty_identities() {
    assert!(matches!(
        PrivilegeGrant::new(
            PrincipalId::from_bytes([0; 16]),
            PrivilegeClass::SecurityAdmin,
            None,
        ),
        Err(PrivilegeGrantError::EmptyGrantee)
    ));
    assert!(matches!(
        PrivilegeGrant::new(
            USER,
            PrivilegeClass::Execute,
            Some(FunctionId::from_bytes([0; 16])),
        ),
        Err(PrivilegeGrantError::EmptyObject)
    ));
    assert!(matches!(
        PrivilegeGrant::new(USER, PrivilegeClass::SecurityAdmin, Some(FUNCTION)),
        Err(PrivilegeGrantError::SecurityAdminObject)
    ));
}

#[test]
fn privilege_grant_accessors_expose_grantee_class_and_object() {
    let class_wide = PrivilegeGrant::new(USER, PrivilegeClass::SecurityAdmin, None)
        .expect("a class-wide grant is valid");
    assert_eq!(class_wide.grantee(), USER);
    assert_eq!(class_wide.class(), PrivilegeClass::SecurityAdmin);
    assert_eq!(class_wide.object(), None);
    assert!(class_wide.is_class_wide());

    let scoped = PrivilegeGrant::new(USER, PrivilegeClass::Execute, Some(FUNCTION))
        .expect("an object-scoped grant is valid");
    assert_eq!(scoped.grantee(), USER);
    assert_eq!(scoped.class(), PrivilegeClass::Execute);
    assert_eq!(scoped.object(), Some(FUNCTION));
    assert!(!scoped.is_class_wide());
}

#[test]
fn authorise_privilege_allows_when_the_class_is_granted() {
    assert_eq!(
        authorise_privilege(
            USER,
            PrivilegeClass::Execute,
            None,
            &[PrivilegeClass::Execute]
        ),
        PrivilegeDecision::Allowed {
            requested: PrivilegeClass::Execute,
        }
    );
    // A class-wide grant covers an object-scoped request.
    assert_eq!(
        authorise_privilege(
            USER,
            PrivilegeClass::Execute,
            Some(FUNCTION),
            &[PrivilegeClass::Execute],
        ),
        PrivilegeDecision::Allowed {
            requested: PrivilegeClass::Execute,
        }
    );
    assert_eq!(
        authorise_privilege(
            USER,
            PrivilegeClass::SecurityAdmin,
            None,
            &[PrivilegeClass::SecurityAdmin],
        ),
        PrivilegeDecision::Allowed {
            requested: PrivilegeClass::SecurityAdmin,
        }
    );
    assert_eq!(
        authorise_privilege(
            USER,
            PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation),
            None,
            &[PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation)],
        ),
        PrivilegeDecision::Allowed {
            requested: PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation),
        }
    );
}

#[test]
fn authorise_privilege_denies_when_the_class_is_not_granted() {
    for requested in [
        PrivilegeClass::Execute,
        PrivilegeClass::SecurityAdmin,
        PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation),
    ] {
        assert_eq!(
            authorise_privilege(USER, requested, None, &[]),
            PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { requested })
        );
    }
    // An unrelated granted class never covers another class.
    assert!(matches!(
        authorise_privilege(
            USER,
            PrivilegeClass::SecurityAdmin,
            None,
            &[PrivilegeClass::Execute],
        ),
        PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { .. })
    ));
    assert!(matches!(
        authorise_privilege(
            USER,
            PrivilegeClass::Execute,
            Some(FUNCTION),
            &[PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation)],
        ),
        PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { .. })
    ));
    assert_eq!(
        authorise_privilege(
            USER,
            PrivilegeClass::SecurityAdmin,
            Some(FUNCTION),
            &[PrivilegeClass::SecurityAdmin],
        ),
        PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege {
            requested: PrivilegeClass::SecurityAdmin,
        })
    );
}

#[test]
fn authorise_privilege_applies_the_inspect_ladder() {
    // A granted higher rung reaches a requested lower rung.
    assert_eq!(
        authorise_privilege(
            USER,
            PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation),
            None,
            &[PrivilegeClass::Inspect(
                InspectPrivilege::SessionInvocations
            )],
        ),
        PrivilegeDecision::Allowed {
            requested: PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation),
        }
    );
    assert_eq!(
        authorise_privilege(
            USER,
            PrivilegeClass::Inspect(InspectPrivilege::SessionInvocations),
            None,
            &[PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation)],
        ),
        PrivilegeDecision::Allowed {
            requested: PrivilegeClass::Inspect(InspectPrivilege::SessionInvocations),
        }
    );
    // A requested rung above the granted rung is denied.
    assert!(matches!(
        authorise_privilege(
            USER,
            PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation),
            None,
            &[PrivilegeClass::Inspect(
                InspectPrivilege::SessionInvocations
            )],
        ),
        PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { .. })
    ));
}

#[test]
fn authorise_privilege_applies_the_inspect_classifier_matrix() {
    // A classifier request without any ladder rung is denied.
    assert!(matches!(
        authorise_privilege(
            USER,
            PrivilegeClass::Inspect(InspectPrivilege::Values),
            None,
            &[PrivilegeClass::Inspect(InspectPrivilege::Values)],
        ),
        PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { .. })
    ));
    // A ladder rung plus the same classifier allows the request.
    assert_eq!(
        authorise_privilege(
            USER,
            PrivilegeClass::Inspect(InspectPrivilege::Values),
            None,
            &[
                PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation),
                PrivilegeClass::Inspect(InspectPrivilege::Values),
            ],
        ),
        PrivilegeDecision::Allowed {
            requested: PrivilegeClass::Inspect(InspectPrivilege::Values),
        }
    );
    // A different classifier never covers the requested one.
    assert!(matches!(
        authorise_privilege(
            USER,
            PrivilegeClass::Inspect(InspectPrivilege::Values),
            None,
            &[
                PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation),
                PrivilegeClass::Inspect(InspectPrivilege::Source),
            ],
        ),
        PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { .. })
    ));
    // A classifier never grants a ladder rung.
    assert!(matches!(
        authorise_privilege(
            USER,
            PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation),
            None,
            &[PrivilegeClass::Inspect(InspectPrivilege::Values)],
        ),
        PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { .. })
    ));
}

#[test]
fn privilege_denials_carry_closed_audit_reasons() {
    assert_eq!(
        PrivilegeDenial::MissingPrivilege {
            requested: PrivilegeClass::Execute,
        }
        .audit_reason(),
        "execute:missing-privilege"
    );
    assert_eq!(
        PrivilegeDenial::MissingPrivilege {
            requested: PrivilegeClass::SecurityAdmin,
        }
        .audit_reason(),
        "security_admin:missing-privilege"
    );
    assert_eq!(
        PrivilegeDenial::MissingPrivilege {
            requested: PrivilegeClass::Inspect(InspectPrivilege::Values),
        }
        .audit_reason(),
        "inspect:missing-privilege"
    );
}

#[test]
fn snapshot_privilege_grants_round_trip_through_the_deepest_constructor() {
    let class_wide = PrivilegeGrant::new(USER, PrivilegeClass::SecurityAdmin, None)
        .expect("a class-wide grant is valid");
    let scoped = PrivilegeGrant::new(USER, PrivilegeClass::Execute, Some(FUNCTION))
        .expect("a known object-scoped grant is valid");
    let snapshot =
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![SecurityFunctionTarget::application(FUNCTION)],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![],
            vec![],
            vec![class_wide, scoped],
        )
        .expect("a snapshot with privilege grants is valid");
    assert_eq!(
        snapshot.privilege_grants().collect::<Vec<_>>(),
        vec![scoped, class_wide]
    );

    // The stable existing constructors default to no privilege grants.
    let plain = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("the flat constructor stays valid");
    assert_eq!(plain.privilege_grants().count(), 0);
}

#[test]
fn snapshot_rejects_privilege_grants_for_unknown_objects() {
    let grant = PrivilegeGrant::new(USER, PrivilegeClass::Execute, Some(OTHER_FUNCTION))
        .expect("an unknown object-scoped grant still has a valid grant shape");
    assert!(matches!(
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![SecurityFunctionTarget::application(FUNCTION)],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![],
            vec![],
            vec![grant],
        ),
        Err(SecuritySnapshotError::UnknownPrivilegeGrantObject)
    ));
}

#[test]
fn snapshot_accepts_privilege_grants_for_sealed_system_functions() {
    let grant = PrivilegeGrant::new(
        USER,
        PrivilegeClass::Execute,
        Some(SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID),
    )
    .expect("a sealed system function object has a valid grant shape");
    let snapshot =
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![],
            vec![],
            vec![grant],
        )
        .expect("sealed system function objects are valid privilege targets");
    assert_eq!(snapshot.privilege_grants().collect::<Vec<_>>(), vec![grant]);
}

#[test]
fn snapshot_rejects_duplicate_privilege_grants() {
    let grant = PrivilegeGrant::new(USER, PrivilegeClass::SecurityAdmin, None)
        .expect("a class-wide grant is valid");
    assert!(matches!(
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![SecurityFunctionTarget::application(FUNCTION)],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![],
            vec![],
            vec![grant, grant],
        ),
        Err(SecuritySnapshotError::DuplicatePrivilegeGrant)
    ));
}

#[test]
fn snapshot_rejects_privilege_grants_for_unknown_principals() {
    let grant = PrivilegeGrant::new(OTHER_PRINCIPAL, PrivilegeClass::Execute, None)
        .expect("a class-wide grant is valid");
    assert!(matches!(
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![SecurityFunctionTarget::application(FUNCTION)],
            vec![active(USER, PrincipalKind::User)],
            vec![],
            vec![],
            vec![],
            vec![grant],
        ),
        Err(SecuritySnapshotError::UnknownPrivilegeGrantPrincipal)
    ));
}

#[test]
fn security_admin_audit_decisions_record_operation_and_target() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid security-admin audit session");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");

    let decision = authorise_privilege(
        USER,
        PrivilegeClass::SecurityAdmin,
        None,
        &[PrivilegeClass::SecurityAdmin],
    );
    let allowed = SecurityAuditDecision::security_admin_allowed(
        &session,
        decision,
        SecurityAdminAuditOperation::GrantRole,
        SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
    )
    .expect("an allowed decision must record");
    assert_eq!(allowed.kind(), SecurityAuditKind::SecurityAdmin);
    assert_eq!(allowed.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(allowed.session_principal(), Some(USER));
    assert_eq!(
        allowed.security_admin_operation(),
        Some(SecurityAdminAuditOperation::GrantRole)
    );
    assert_eq!(
        allowed.security_admin_target(),
        Some(SYS_SECURITY_GRANT_ROLE_FUNCTION_ID)
    );
    assert_eq!(allowed.security_admin_denial(), None);
    assert_eq!(allowed.denial(), None);

    let recovered = SecurityAuditDecision::recover_security_admin_allowed(
        USER,
        SecurityAdminAuditOperation::CreatePrincipal,
        SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
    );
    assert_eq!(recovered.kind(), SecurityAuditKind::SecurityAdmin);
    assert_eq!(
        recovered.security_admin_operation(),
        Some(SecurityAdminAuditOperation::CreatePrincipal)
    );
    assert_eq!(
        recovered.security_admin_target(),
        Some(SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID)
    );

    let denied = SecurityAuditDecision::security_admin_denied(
        &session,
        SecurityAdminAuditOperation::GrantPrivilege,
        SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
        PrivilegeDenial::MissingPrivilege {
            requested: PrivilegeClass::SecurityAdmin,
        },
    );
    assert_eq!(denied.kind(), SecurityAuditKind::SecurityAdmin);
    assert_eq!(denied.outcome(), SecurityAuditOutcome::Denied);
    assert_eq!(denied.session_principal(), Some(USER));
    assert_eq!(
        denied.denial(),
        Some(SecurityAuditDenial::SecurityAdmin(
            PrivilegeDenial::MissingPrivilege {
                requested: PrivilegeClass::SecurityAdmin,
            }
        ))
    );
    assert_eq!(
        denied.security_admin_denial(),
        Some(PrivilegeDenial::MissingPrivilege {
            requested: PrivilegeClass::SecurityAdmin,
        })
    );
    assert_eq!(
        denied.security_admin_operation(),
        Some(SecurityAdminAuditOperation::GrantPrivilege)
    );
    assert_eq!(
        denied.security_admin_target(),
        Some(SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID)
    );
}

#[test]
fn source_apply_audit_records_only_trusted_session_and_candidate() {
    let decision = SecurityAuditDecision::recover_source_apply_allowed(USER, OTHER_REVISION);

    assert_eq!(decision.kind(), SecurityAuditKind::SourceApply);
    assert_eq!(decision.outcome(), SecurityAuditOutcome::Allowed);
    assert_eq!(decision.session_principal(), Some(USER));
    assert_eq!(decision.source_apply_candidate(), Some(OTHER_REVISION));
    assert_eq!(decision.target(), None);
    assert_eq!(decision.denial(), None);
}

#[test]
fn security_admin_audit_allowed_rejects_wrong_decision_shapes() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![active(USER, PrincipalKind::User)],
        vec![],
        vec![],
    )
    .expect("valid security-admin audit session");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");
    assert_eq!(
        SecurityAuditDecision::security_admin_allowed(
            &session,
            PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege {
                requested: PrivilegeClass::SecurityAdmin,
            }),
            SecurityAdminAuditOperation::CreateRole,
            SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
        ),
        Err(SecurityAuditDecisionError::SecurityAdminDecisionShape)
    );
    // An allowed decision for a different class never records as admin.
    assert_eq!(
        SecurityAuditDecision::security_admin_allowed(
            &session,
            PrivilegeDecision::Allowed {
                requested: PrivilegeClass::Execute,
            },
            SecurityAdminAuditOperation::CreateRole,
            SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
        ),
        Err(SecurityAuditDecisionError::SecurityAdminDecisionShape)
    );
}
