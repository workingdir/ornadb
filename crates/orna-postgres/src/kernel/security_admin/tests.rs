
use orna_core::{
    CatalogueRevisionId, FunctionId, PrincipalId, SourceRevisionId,
    inspect::InspectPrivilege,
    revision::RevisionPair,
    security::{
        CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, Principal, PrincipalKind, PrincipalStatus,
        PrivilegeClass, PrivilegeGrant, RoleMembership, SecuritySnapshot, SecuritySnapshotError,
    },
    system::{
        SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID, SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
    },
};

use super::{PostgresKernelError, SecurityAdminMutation, rebuild_candidate};

const USER: PrincipalId = PrincipalId::from_bytes([0x91; 16]);
const ROLE: PrincipalId = PrincipalId::from_bytes([0x95; 16]);
const OTHER_ROLE: PrincipalId = PrincipalId::from_bytes([0x96; 16]);
const MEMBER: PrincipalId = PrincipalId::from_bytes([0x97; 16]);
const UNKNOWN_ROLE: PrincipalId = PrincipalId::from_bytes([0x98; 16]);
const UNKNOWN_MEMBER: PrincipalId = PrincipalId::from_bytes([0x99; 16]);
const UNKNOWN_OBJECT: FunctionId = FunctionId::from_bytes([0x92; 16]);
const REVISION: RevisionPair = RevisionPair::new(
    SourceRevisionId::from_bytes([0x93; 16]),
    CatalogueRevisionId::from_bytes([0x94; 16]),
);

fn snapshot() -> SecuritySnapshot {
    SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
        REVISION,
        vec![],
        vec![Principal::new(
            USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .expect("the focused security-admin snapshot should be valid")
}

fn snapshot_with_reserved_service() -> SecuritySnapshot {
    SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
        REVISION,
        vec![],
        vec![
            Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
            Principal::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                PrincipalKind::Service,
                PrincipalStatus::Active,
            ),
        ],
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .expect("the reserved service security-admin snapshot should be valid")
}

fn rebuilt_with_grants(
    current: &SecuritySnapshot,
    grants: Vec<PrivilegeGrant>,
) -> SecuritySnapshot {
    rebuild_candidate(
        current,
        current.principals().collect(),
        current.memberships().collect(),
        grants,
    )
    .expect("the candidate privilege grants should rebuild")
}

fn grant_role_candidate_error(
    current: &SecuritySnapshot,
    role: PrincipalId,
    member: PrincipalId,
) -> PostgresKernelError {
    let mutation = SecurityAdminMutation::GrantRole { role, member }
        .validate(current)
        .expect("candidate-only GrantRole invariants should pass input validation");
    let SecurityAdminMutation::GrantRole { role, member } = mutation else {
        unreachable!("GrantRole validation must preserve the mutation kind")
    };
    let mut memberships = current.memberships().collect::<Vec<_>>();
    memberships.push(RoleMembership::new(role, member));
    rebuild_candidate(
        current,
        current.principals().collect(),
        memberships,
        current.privilege_grants().collect(),
    )
    .expect_err("malformed GrantRole input must fail candidate snapshot rebuild")
}

#[test]
fn security_admin_sealed_object_grants_round_trip_and_unknown_objects_fail_closed() {
    let current = snapshot();
    let execute = PrivilegeGrant::new(
        USER,
        PrivilegeClass::Execute,
        Some(SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID),
    )
    .expect("the sealed security admin target has a valid identity");
    let inspect = PrivilegeGrant::new(
        USER,
        PrivilegeClass::Inspect(InspectPrivilege::Values),
        Some(SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID),
    )
    .expect("the sealed inspect target has a valid identity");

    SecurityAdminMutation::GrantPrivilege {
        grantee: USER,
        class: execute.class(),
        object: execute.object(),
    }
    .validate(&current)
    .expect("sealed EXECUTE object should pass admin validation");
    SecurityAdminMutation::GrantPrivilege {
        grantee: USER,
        class: inspect.class(),
        object: inspect.object(),
    }
    .validate(&current)
    .expect("sealed INSPECT object should pass admin validation");

    let granted = rebuilt_with_grants(&current, vec![execute, inspect]);
    assert_eq!(
        granted
            .privilege_grants()
            .find(|grant| grant.class() == PrivilegeClass::Execute)
            .and_then(|grant| grant.object()),
        Some(SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID)
    );
    assert_eq!(
        granted
            .privilege_grants()
            .find(|grant| matches!(grant.class(), PrivilegeClass::Inspect(_)))
            .and_then(|grant| grant.object()),
        Some(SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID)
    );

    SecurityAdminMutation::RevokePrivilege {
        grantee: USER,
        class: execute.class(),
        object: execute.object(),
    }
    .validate(&granted)
    .expect("sealed EXECUTE object should pass revoke validation");
    let revoked = rebuilt_with_grants(
        &granted,
        granted
            .privilege_grants()
            .filter(|grant| *grant != execute)
            .collect(),
    );
    assert!(!revoked.privilege_grants().any(|grant| grant == execute));
    assert!(revoked.privilege_grants().any(|grant| grant == inspect));

    for mutation in [
        SecurityAdminMutation::GrantPrivilege {
            grantee: USER,
            class: PrivilegeClass::Execute,
            object: Some(UNKNOWN_OBJECT),
        },
        SecurityAdminMutation::RevokePrivilege {
            grantee: USER,
            class: PrivilegeClass::Inspect(InspectPrivilege::Values),
            object: Some(UNKNOWN_OBJECT),
        },
    ] {
        assert!(matches!(
            mutation.validate(&granted),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_privilege_grants",
                record,
                rule: "the privilege grant object must exist",
            }) if record == "grant_privilege" || record == "revoke_privilege"
        ));
    }

    let malformed = SecurityAdminMutation::RevokePrivilege {
        grantee: USER,
        class: PrivilegeClass::Execute,
        object: Some(FunctionId::from_bytes([0; 16])),
    };
    assert!(matches!(
        malformed.validate(&granted),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_privilege_grants",
            record,
            rule: "the privilege grant object must not be the empty identity",
        }) if record == "revoke_privilege"
    ));
}

#[test]
fn reserved_catalogue_health_service_identity_rejects_privilege_grants() {
    let current = snapshot();
    let attempts = [
        (PrivilegeClass::SecurityAdmin, None),
        (PrivilegeClass::Execute, None),
        (
            PrivilegeClass::Execute,
            Some(SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID),
        ),
        (PrivilegeClass::Inspect(InspectPrivilege::Values), None),
        (
            PrivilegeClass::Inspect(InspectPrivilege::Values),
            Some(SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID),
        ),
    ];

    for (class, object) in attempts {
        let grant = PrivilegeGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, class, object)
            .expect("reserved identity grant test input should be structurally valid");
        assert!(matches!(
            SecurityAdminMutation::GrantPrivilege {
                grantee: grant.grantee(),
                class: grant.class(),
                object: grant.object(),
            }
            .validate(&current),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_privilege_grants",
                record,
                rule: "the reserved catalogue health service identity cannot receive privilege grants",
            }) if record == "grant_privilege"
        ));
    }
}

#[test]
fn reserved_catalogue_health_service_identity_revoke_remains_available_for_cleanup() {
    let current = snapshot_with_reserved_service();
    let revoked = PrivilegeGrant::new(
        CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
        PrivilegeClass::Execute,
        None,
    )
    .expect("reserved identity revoke test input should be structurally valid");

    SecurityAdminMutation::RevokePrivilege {
        grantee: revoked.grantee(),
        class: revoked.class(),
        object: revoked.object(),
    }
    .validate(&current)
    .expect("reserved identity privilege revoke should remain available for cleanup");
}

#[test]
fn reserved_catalogue_health_service_identity_rejects_role_membership() {
    let current = snapshot();
    let membership = RoleMembership::new(ROLE, CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);

    assert!(matches!(
        SecurityAdminMutation::GrantRole {
            role: membership.role(),
            member: membership.member(),
        }
        .validate(&current),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_role_memberships",
            record,
            rule: "the reserved catalogue health service identity cannot become a role member",
        }) if record == "grant_role"
    ));
}

#[test]
fn create_principal_rejects_empty_identity_before_candidate_mutation() {
    let current = snapshot();
    let result = SecurityAdminMutation::CreatePrincipal {
        principal: PrincipalId::from_bytes([0; 16]),
        kind: PrincipalKind::User,
    }
    .validate(&current);
    assert!(matches!(
        result,
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record,
            rule: "the principal identity must not be the empty identity",
        }) if record == "create_principal"
    ));
    assert!(
        !current
            .principals()
            .any(|principal| principal.id() == PrincipalId::from_bytes([0; 16]))
    );
}

#[test]
fn recovered_candidate_rejects_empty_principal_before_persistence() {
    let current = snapshot();
    let mut principals = current.principals().collect::<Vec<_>>();
    principals.push(Principal::new(
        PrincipalId::from_bytes([0; 16]),
        PrincipalKind::User,
        PrincipalStatus::Active,
    ));

    assert!(matches!(
        rebuild_candidate(
            &current,
            principals,
            current.memberships().collect(),
            current.privilege_grants().collect(),
        ),
        Err(PostgresKernelError::SecuritySnapshot(
            SecuritySnapshotError::EmptyPrincipal
        ))
    ));
}

#[test]
fn create_role_rejects_empty_identity_before_candidate_mutation() {
    let current = snapshot();
    let result = SecurityAdminMutation::CreateRole {
        role: PrincipalId::from_bytes([0; 16]),
    }
    .validate(&current);
    assert!(matches!(
        result,
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record,
            rule: "the principal identity must not be the empty identity",
        }) if record == "create_role"
    ));
    assert!(
        !current
            .principals()
            .any(|principal| principal.id() == PrincipalId::from_bytes([0; 16]))
    );
}

#[test]
fn non_reserved_service_principal_validates_and_persists_active_candidate() {
    let current = snapshot();
    let service = PrincipalId::from_bytes([0x9a; 16]);
    assert_ne!(service, CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);

    let mutation = SecurityAdminMutation::CreatePrincipal {
        principal: service,
        kind: PrincipalKind::Service,
    }
    .validate(&current)
    .expect("a non-reserved service principal should pass admin validation");
    let SecurityAdminMutation::CreatePrincipal { principal, kind } = mutation else {
        unreachable!("CreatePrincipal validation must preserve the mutation kind")
    };

    let mut principals = current.principals().collect::<Vec<_>>();
    principals.push(Principal::new(principal, kind, PrincipalStatus::Active));
    let candidate = rebuild_candidate(
        &current,
        principals,
        current.memberships().collect(),
        current.privilege_grants().collect(),
    )
    .expect("the service principal candidate should rebuild");

    assert!(
        !current
            .principals()
            .any(|candidate| candidate.id() == service)
    );
    let persisted = candidate
        .principals()
        .find(|candidate| candidate.id() == service)
        .expect("the rebuilt candidate should persist the service principal");
    assert_eq!(persisted.kind(), PrincipalKind::Service);
    assert_eq!(persisted.status(), PrincipalStatus::Active);
}

#[test]
fn malformed_grant_role_inputs_fail_at_candidate_validation_boundary() {
    let unknown_role = grant_role_candidate_error(&snapshot(), UNKNOWN_ROLE, USER);
    assert!(matches!(
        unknown_role,
        PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::UnknownMembershipRole)
    ));

    let known_user_as_role = grant_role_candidate_error(
        &SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![],
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(MEMBER, PrincipalKind::User, PrincipalStatus::Active),
            ],
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .expect("known-user role-target fixture should be valid"),
        USER,
        MEMBER,
    );
    assert!(matches!(
        known_user_as_role,
        PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::MembershipTargetIsNotRole)
    ));

    let unknown_member = grant_role_candidate_error(
        &SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![],
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .expect("unknown-member fixture should be valid"),
        ROLE,
        UNKNOWN_MEMBER,
    );
    assert!(matches!(
        unknown_member,
        PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::UnknownMembershipMember)
    ));

    let self_membership = grant_role_candidate_error(
        &SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![],
            vec![Principal::new(
                ROLE,
                PrincipalKind::Role,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .expect("self-membership fixture should be valid"),
        ROLE,
        ROLE,
    );
    assert!(matches!(
        self_membership,
        PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::SelfMembership)
    ));

    let indirect_cycle = grant_role_candidate_error(
        &SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            REVISION,
            vec![],
            vec![
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                Principal::new(OTHER_ROLE, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(ROLE, OTHER_ROLE)],
            vec![],
            vec![],
            vec![],
        )
        .expect("indirect-cycle fixture should be valid"),
        OTHER_ROLE,
        ROLE,
    );
    assert!(matches!(
        indirect_cycle,
        PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::CyclicRoleMembership)
    ));
}
