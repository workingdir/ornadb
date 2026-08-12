//! Deny-by-default decisions for authenticated function execution.

#![deny(missing_docs)]

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use crate::{FunctionId, PrincipalId, revision::RevisionPair};

/// The security-relevant kind of an Orna principal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrincipalKind {
    /// A human login identity.
    User,
    /// A principal selected through role membership.
    Role,
    /// A non-human login identity.
    Service,
}

/// Whether a principal can participate in a security decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrincipalStatus {
    /// The principal can authenticate or be selected as a role.
    Active,
    /// The principal is retained but grants no authority.
    Disabled,
}

/// One principal in a validated security snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Principal {
    id: PrincipalId,
    kind: PrincipalKind,
    status: PrincipalStatus,
}

impl Principal {
    /// Creates a principal record.
    pub const fn new(id: PrincipalId, kind: PrincipalKind, status: PrincipalStatus) -> Self {
        Self { id, kind, status }
    }

    /// Returns the stable principal identity.
    pub const fn id(self) -> PrincipalId {
        self.id
    }

    /// Returns the principal kind.
    pub const fn kind(self) -> PrincipalKind {
        self.kind
    }

    /// Returns whether the principal is active or disabled.
    pub const fn status(self) -> PrincipalStatus {
        self.status
    }
}

/// A directed membership from one principal to a containing role.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RoleMembership {
    role: PrincipalId,
    member: PrincipalId,
}

impl RoleMembership {
    /// Creates a membership equivalent to `GRANT role TO member`.
    pub const fn new(role: PrincipalId, member: PrincipalId) -> Self {
        Self { role, member }
    }

    /// Returns the containing role.
    pub const fn role(self) -> PrincipalId {
        self.role
    }

    /// Returns the principal contained by the role.
    pub const fn member(self) -> PrincipalId {
        self.member
    }
}

/// A function-specific `EXECUTE` grant.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExecuteGrant {
    grantee: PrincipalId,
    function: FunctionId,
}

impl ExecuteGrant {
    /// Creates a direct `EXECUTE` grant.
    pub const fn new(grantee: PrincipalId, function: FunctionId) -> Self {
        Self { grantee, function }
    }

    /// Returns the principal that received the grant.
    pub const fn grantee(self) -> PrincipalId {
        self.grantee
    }

    /// Returns the function covered by the grant.
    pub const fn function(self) -> FunctionId {
        self.function
    }
}

/// A function and revision pair selected by the server for one invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvocationTarget {
    function: FunctionId,
    revision: RevisionPair,
}

impl InvocationTarget {
    /// Creates a pinned function target.
    pub const fn new(function: FunctionId, revision: RevisionPair) -> Self {
        Self { function, revision }
    }

    /// Returns the selected function identity.
    pub const fn function(self) -> FunctionId {
        self.function
    }

    /// Returns the exact pinned revision pair.
    pub const fn revision(self) -> RevisionPair {
        self.revision
    }
}

/// A session identity bound from trusted authentication state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedSession {
    principal: PrincipalId,
    active_roles: Vec<PrincipalId>,
}

impl AuthenticatedSession {
    /// Returns the principal established by trusted authentication.
    pub const fn principal(&self) -> PrincipalId {
        self.principal
    }

    /// Returns the explicitly selected active roles in canonical order.
    pub fn active_roles(&self) -> &[PrincipalId] {
        &self.active_roles
    }
}

/// Invalid trusted session state presented to a security snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionBindingError {
    /// The authenticated principal is absent from the snapshot.
    UnknownSessionPrincipal,
    /// The authenticated principal is disabled.
    DisabledSessionPrincipal,
    /// A role cannot authenticate as a session principal.
    RoleCannotAuthenticate,
    /// The active-role list repeats an identity.
    DuplicateActiveRole,
    /// A selected active role is absent from the snapshot.
    UnknownActiveRole,
    /// A selected active role is disabled.
    DisabledActiveRole,
    /// A selected identity is not a role.
    ActivePrincipalIsNotRole,
    /// The session principal cannot reach the selected role.
    UnreachableActiveRole,
}

impl fmt::Display for SessionBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownSessionPrincipal => "authenticated principal is unknown",
            Self::DisabledSessionPrincipal => "authenticated principal is disabled",
            Self::RoleCannotAuthenticate => "a role cannot authenticate",
            Self::DuplicateActiveRole => "an active role is repeated",
            Self::UnknownActiveRole => "an active role is unknown",
            Self::DisabledActiveRole => "an active role is disabled",
            Self::ActivePrincipalIsNotRole => "an active principal is not a role",
            Self::UnreachableActiveRole => "an active role is not reachable",
        })
    }
}

impl Error for SessionBindingError {}

/// An invariant violation in recovered or newly prepared security state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecuritySnapshotError {
    /// Two principal records use the same identity.
    DuplicatePrincipal,
    /// Two known-function records use the same identity.
    DuplicateFunction,
    /// Two membership records describe the same edge.
    DuplicateMembership,
    /// Two grants name the same grantee and function.
    DuplicateExecuteGrant,
    /// A membership names a member absent from the snapshot.
    UnknownMembershipMember,
    /// A membership names a containing principal absent from the snapshot.
    UnknownMembershipRole,
    /// A membership target exists but is not a role.
    MembershipTargetIsNotRole,
    /// A role contains itself directly.
    SelfMembership,
    /// The role-membership graph contains an indirect cycle.
    CyclicRoleMembership,
    /// A grant names a principal absent from the snapshot.
    UnknownGrantPrincipal,
    /// A grant names a function absent from the snapshot.
    UnknownGrantFunction,
}

impl fmt::Display for SecuritySnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DuplicatePrincipal => "security snapshot contains a duplicate principal",
            Self::DuplicateFunction => "security snapshot contains a duplicate function",
            Self::DuplicateMembership => "security snapshot contains a duplicate membership",
            Self::DuplicateExecuteGrant => "security snapshot contains a duplicate EXECUTE grant",
            Self::UnknownMembershipMember => "security snapshot membership has an unknown member",
            Self::UnknownMembershipRole => "security snapshot membership has an unknown role",
            Self::MembershipTargetIsNotRole => "security snapshot membership target is not a role",
            Self::SelfMembership => "security snapshot contains a self-membership",
            Self::CyclicRoleMembership => "security snapshot contains cyclic role membership",
            Self::UnknownGrantPrincipal => "security snapshot grant has an unknown principal",
            Self::UnknownGrantFunction => "security snapshot grant has an unknown function",
        })
    }
}

impl Error for SecuritySnapshotError {}

/// Evidence that a pinned function invocation is authorised.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorisedInvocation {
    session_principal: PrincipalId,
    effective_principal: PrincipalId,
    active_roles: Vec<PrincipalId>,
    authorising_principal: PrincipalId,
    target: InvocationTarget,
}

impl AuthorisedInvocation {
    /// Returns the authenticated session principal.
    pub const fn session_principal(&self) -> PrincipalId {
        self.session_principal
    }

    /// Returns the principal used to execute the function body.
    pub const fn effective_principal(&self) -> PrincipalId {
        self.effective_principal
    }

    /// Returns the explicitly selected active roles in canonical order.
    pub fn active_roles(&self) -> &[PrincipalId] {
        &self.active_roles
    }

    /// Returns the direct principal or selected role whose grant allowed execution.
    pub const fn authorising_principal(&self) -> PrincipalId {
        self.authorising_principal
    }

    /// Returns the exact target covered by this decision.
    pub const fn target(&self) -> InvocationTarget {
        self.target
    }
}

/// The reason a function invocation was denied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecuteDenial {
    /// The session was not valid in this snapshot.
    InvalidSession,
    /// The snapshot does not contain the requested function.
    UnknownFunction,
    /// The requested revision pair is not the snapshot's active pair.
    RevisionMismatch,
    /// No direct or selected-role grant authorises the function.
    MissingExecuteGrant,
}

/// The complete result of an `EXECUTE` authorisation check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecuteDecision {
    /// Execution is allowed with immutable decision evidence.
    Allowed(AuthorisedInvocation),
    /// Execution is denied without entering an evaluator or executor.
    Denied(ExecuteDenial),
}

/// An immutable, validated view of security and function identities.
#[derive(Clone, Debug)]
pub struct SecuritySnapshot {
    revision: RevisionPair,
    functions: HashSet<FunctionId>,
    principals: HashMap<PrincipalId, Principal>,
    memberships: Vec<RoleMembership>,
    grants: HashSet<ExecuteGrant>,
}

fn role_graph_has_cycle(
    memberships: &[RoleMembership],
    principals: &HashMap<PrincipalId, Principal>,
) -> bool {
    fn visit(
        role: PrincipalId,
        memberships: &[RoleMembership],
        visiting: &mut HashSet<PrincipalId>,
        visited: &mut HashSet<PrincipalId>,
    ) -> bool {
        if visited.contains(&role) {
            return false;
        }
        if !visiting.insert(role) {
            return true;
        }

        for containing_role in memberships
            .iter()
            .filter(|membership| membership.member == role)
            .map(|membership| membership.role)
        {
            if visit(containing_role, memberships, visiting, visited) {
                return true;
            }
        }

        visiting.remove(&role);
        visited.insert(role);
        false
    }

    let mut roles = principals
        .values()
        .filter(|principal| principal.kind == PrincipalKind::Role)
        .map(|principal| principal.id)
        .collect::<Vec<_>>();
    roles.sort_unstable();

    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    roles
        .into_iter()
        .any(|role| visit(role, memberships, &mut visiting, &mut visited))
}

impl SecuritySnapshot {
    /// Creates a security snapshot.
    pub fn new(
        revision: RevisionPair,
        functions: Vec<FunctionId>,
        principals: Vec<Principal>,
        memberships: Vec<RoleMembership>,
        grants: Vec<ExecuteGrant>,
    ) -> Result<Self, SecuritySnapshotError> {
        let mut known_functions = HashSet::new();
        for function in functions {
            if !known_functions.insert(function) {
                return Err(SecuritySnapshotError::DuplicateFunction);
            }
        }

        let mut principals_by_id = HashMap::new();
        for principal in principals {
            if principals_by_id.insert(principal.id, principal).is_some() {
                return Err(SecuritySnapshotError::DuplicatePrincipal);
            }
        }

        let mut membership_set = HashSet::new();
        let mut validated_memberships = Vec::with_capacity(memberships.len());
        for membership in memberships {
            if !membership_set.insert(membership) {
                return Err(SecuritySnapshotError::DuplicateMembership);
            }
            if !principals_by_id.contains_key(&membership.member) {
                return Err(SecuritySnapshotError::UnknownMembershipMember);
            }
            let role = principals_by_id
                .get(&membership.role)
                .ok_or(SecuritySnapshotError::UnknownMembershipRole)?;
            if role.kind != PrincipalKind::Role {
                return Err(SecuritySnapshotError::MembershipTargetIsNotRole);
            }
            if membership.role == membership.member {
                return Err(SecuritySnapshotError::SelfMembership);
            }
            validated_memberships.push(membership);
        }
        validated_memberships
            .sort_unstable_by_key(|membership| (membership.member, membership.role));
        if role_graph_has_cycle(&validated_memberships, &principals_by_id) {
            return Err(SecuritySnapshotError::CyclicRoleMembership);
        }

        let mut validated_grants = HashSet::new();
        for grant in grants {
            if !validated_grants.insert(grant) {
                return Err(SecuritySnapshotError::DuplicateExecuteGrant);
            }
            if !principals_by_id.contains_key(&grant.grantee) {
                return Err(SecuritySnapshotError::UnknownGrantPrincipal);
            }
            if !known_functions.contains(&grant.function) {
                return Err(SecuritySnapshotError::UnknownGrantFunction);
            }
        }

        Ok(Self {
            revision,
            functions: known_functions,
            principals: principals_by_id,
            memberships: validated_memberships,
            grants: validated_grants,
        })
    }

    /// Binds trusted authentication state to this snapshot.
    pub fn bind_authenticated_session(
        &self,
        principal: PrincipalId,
        mut active_roles: Vec<PrincipalId>,
    ) -> Result<AuthenticatedSession, SessionBindingError> {
        active_roles.sort_unstable();
        if active_roles.windows(2).any(|roles| roles[0] == roles[1]) {
            return Err(SessionBindingError::DuplicateActiveRole);
        }

        let session_principal = self
            .principals
            .get(&principal)
            .ok_or(SessionBindingError::UnknownSessionPrincipal)?;
        if session_principal.status == PrincipalStatus::Disabled {
            return Err(SessionBindingError::DisabledSessionPrincipal);
        }
        if session_principal.kind == PrincipalKind::Role {
            return Err(SessionBindingError::RoleCannotAuthenticate);
        }
        let reachable_roles = self.reachable_roles(principal);
        for active_role in &active_roles {
            let role = self
                .principals
                .get(active_role)
                .ok_or(SessionBindingError::UnknownActiveRole)?;
            if role.status == PrincipalStatus::Disabled {
                return Err(SessionBindingError::DisabledActiveRole);
            }
            if role.kind != PrincipalKind::Role {
                return Err(SessionBindingError::ActivePrincipalIsNotRole);
            }
            if !reachable_roles.contains(active_role) {
                return Err(SessionBindingError::UnreachableActiveRole);
            }
        }

        Ok(AuthenticatedSession {
            principal,
            active_roles,
        })
    }

    fn reachable_roles(&self, principal: PrincipalId) -> HashSet<PrincipalId> {
        let mut reached = HashSet::new();
        let mut pending = vec![principal];

        while let Some(member) = pending.pop() {
            for membership in self
                .memberships
                .iter()
                .filter(|membership| membership.member == member)
            {
                if reached.insert(membership.role) {
                    pending.push(membership.role);
                }
            }
        }

        reached
    }

    /// Decides whether the authenticated session may execute the pinned target.
    pub fn authorise_execute(
        &self,
        session: &AuthenticatedSession,
        target: InvocationTarget,
    ) -> ExecuteDecision {
        if self
            .bind_authenticated_session(session.principal, session.active_roles.clone())
            .is_err()
        {
            return ExecuteDecision::Denied(ExecuteDenial::InvalidSession);
        }
        if target.revision != self.revision {
            return ExecuteDecision::Denied(ExecuteDenial::RevisionMismatch);
        }
        if !self.functions.contains(&target.function) {
            return ExecuteDecision::Denied(ExecuteDenial::UnknownFunction);
        }
        let direct_grant = ExecuteGrant::new(session.principal, target.function);
        let authorising_principal = if self.grants.contains(&direct_grant) {
            session.principal
        } else if let Some(role) = session
            .active_roles
            .iter()
            .copied()
            .filter(|role| {
                self.grants
                    .contains(&ExecuteGrant::new(*role, target.function))
            })
            .min()
        {
            role
        } else {
            return ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant);
        };

        ExecuteDecision::Allowed(AuthorisedInvocation {
            session_principal: session.principal,
            effective_principal: session.principal,
            active_roles: session.active_roles.clone(),
            authorising_principal,
            target,
        })
    }
}

#[cfg(test)]
mod tests {
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
}
