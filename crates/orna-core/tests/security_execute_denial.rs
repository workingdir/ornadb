use orna_core::{
    CatalogueRevisionId, FunctionId, PrincipalId, SourceRevisionId,
    revision::RevisionPair,
    security::{
        ExecuteDenial, InvocationTarget, Principal, PrincipalKind, PrincipalStatus,
        SecurityAuditDecision, SecurityAuditDenial, SecuritySnapshot,
    },
};

const REVISION: RevisionPair = RevisionPair::new(
    SourceRevisionId::from_bytes([0x11; 16]),
    CatalogueRevisionId::from_bytes([0x22; 16]),
);
const USER: PrincipalId = PrincipalId::from_bytes([0x33; 16]);
const FUNCTION: FunctionId = FunctionId::from_bytes([0x44; 16]);

#[test]
fn unsupported_security_definer_denial_is_retained_as_distinct_execute_audit_evidence() {
    let snapshot = SecuritySnapshot::new(
        REVISION,
        vec![FUNCTION],
        vec![Principal::new(
            USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    )
    .expect("valid security snapshot");
    let session = snapshot
        .bind_authenticated_session(USER, vec![])
        .expect("active user session should bind");
    let target = InvocationTarget::new(FUNCTION, REVISION);

    let denied = SecurityAuditDecision::execute_denied(
        &session,
        target,
        ExecuteDenial::UnsupportedSecurityDefiner,
    );

    assert_eq!(denied.target(), Some(target));
    assert_eq!(
        denied.denial(),
        Some(SecurityAuditDenial::Execute(
            ExecuteDenial::UnsupportedSecurityDefiner,
        ))
    );
    assert_ne!(
        denied.denial(),
        Some(SecurityAuditDenial::Execute(ExecuteDenial::UnknownFunction))
    );
}
