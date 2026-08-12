//! Deny-by-default decisions for authenticated function execution.

#![deny(missing_docs)]

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    error::Error,
    fmt,
    time::SystemTime,
};

use crate::{FunctionId, PrincipalId, SecurityAuditEventId, revision::RevisionPair};

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
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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

/// A protected mapping from one Linux peer UID to one Orna principal.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalPeerCredential {
    uid: u32,
    principal: PrincipalId,
}

impl LocalPeerCredential {
    /// Creates a local peer credential mapping.
    pub const fn new(uid: u32, principal: PrincipalId) -> Self {
        Self { uid, principal }
    }

    /// Returns the numeric Linux peer UID.
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the principal selected by this protected mapping.
    pub const fn principal(self) -> PrincipalId {
        self.principal
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

/// A failure to authenticate a kernel-supplied local peer UID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalPeerAuthenticationError {
    /// No protected credential maps this UID to an Orna principal.
    UnknownUid,
    /// The mapped principal cannot create an authenticated session.
    InvalidPrincipal(SessionBindingError),
}

impl fmt::Display for LocalPeerAuthenticationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownUid => formatter.write_str("local peer credential is unknown"),
            Self::InvalidPrincipal(source) => source.fmt(formatter),
        }
    }
}

impl Error for LocalPeerAuthenticationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UnknownUid => None,
            Self::InvalidPrincipal(source) => Some(source),
        }
    }
}

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
    /// Two local credentials use the same Linux UID.
    DuplicateLocalPeerUid,
    /// Two local credentials map to the same principal.
    DuplicateLocalPeerPrincipal,
    /// A local credential names a principal absent from the snapshot.
    UnknownLocalPeerPrincipal,
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
            Self::DuplicateLocalPeerUid => "security snapshot contains a duplicate local peer UID",
            Self::DuplicateLocalPeerPrincipal => {
                "security snapshot contains a duplicate local peer principal"
            }
            Self::UnknownLocalPeerPrincipal => {
                "security snapshot local credential has an unknown principal"
            }
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

/// The closed family of protected security audit events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityAuditKind {
    /// A local connection attempted to establish an authenticated session.
    Authentication,
    /// An authenticated session requested permission to execute a function.
    Execute,
}

/// Whether a protected security decision allowed or denied its operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityAuditOutcome {
    /// The protected decision allowed the operation.
    Allowed,
    /// The protected decision denied the operation.
    Denied,
}

/// The closed reason carried by a denied security audit decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityAuditDenial {
    /// Local peer authentication failed for the recorded reason.
    Authentication(LocalPeerAuthenticationError),
    /// Function execution was denied for the recorded reason.
    Execute(ExecuteDenial),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SecurityAuditDecisionShape {
    AuthenticationAllowed {
        session_principal: PrincipalId,
    },
    AuthenticationDenied {
        session_principal: Option<PrincipalId>,
        reason: LocalPeerAuthenticationError,
    },
    ExecuteAllowed {
        session_principal: PrincipalId,
        effective_principal: PrincipalId,
        authorising_principal: PrincipalId,
        target: InvocationTarget,
    },
    ExecuteDenied {
        session_principal: PrincipalId,
        target: InvocationTarget,
        reason: ExecuteDenial,
    },
}

/// An immutable authentication or `EXECUTE` decision prepared for auditing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecurityAuditDecision(SecurityAuditDecisionShape);

impl SecurityAuditDecision {
    /// Records an allowed authentication decision for one bound session.
    pub fn authentication_allowed(session: &AuthenticatedSession) -> Self {
        Self::recover_authentication_allowed(session.principal)
    }

    /// Recovers an allowed authentication decision from protected storage.
    pub const fn recover_authentication_allowed(session_principal: PrincipalId) -> Self {
        Self(SecurityAuditDecisionShape::AuthenticationAllowed { session_principal })
    }

    /// Records a denied local authentication decision.
    ///
    /// An unknown UID has no principal. A mapping that selects an invalid
    /// principal must retain that principal as evidence.
    pub fn authentication_denied(
        session_principal: Option<PrincipalId>,
        reason: LocalPeerAuthenticationError,
    ) -> Result<Self, SecurityAuditDecisionError> {
        let valid_shape = matches!(
            (session_principal, reason),
            (None, LocalPeerAuthenticationError::UnknownUid)
                | (Some(_), LocalPeerAuthenticationError::InvalidPrincipal(_))
        );
        if !valid_shape {
            return Err(SecurityAuditDecisionError::AuthenticationPrincipalShape);
        }
        Ok(Self(SecurityAuditDecisionShape::AuthenticationDenied {
            session_principal,
            reason,
        }))
    }

    /// Records an allowed `EXECUTE` decision from its immutable evidence.
    pub fn execute_allowed(authorised: &AuthorisedInvocation) -> Self {
        Self::recover_execute_allowed(
            authorised.session_principal,
            authorised.effective_principal,
            authorised.authorising_principal,
            authorised.target,
        )
    }

    /// Recovers an allowed `EXECUTE` decision from protected storage.
    pub const fn recover_execute_allowed(
        session_principal: PrincipalId,
        effective_principal: PrincipalId,
        authorising_principal: PrincipalId,
        target: InvocationTarget,
    ) -> Self {
        Self(SecurityAuditDecisionShape::ExecuteAllowed {
            session_principal,
            effective_principal,
            authorising_principal,
            target,
        })
    }

    /// Records a denied `EXECUTE` decision for a trusted session and target.
    pub fn execute_denied(
        session: &AuthenticatedSession,
        target: InvocationTarget,
        reason: ExecuteDenial,
    ) -> Self {
        Self::recover_execute_denied(session.principal, target, reason)
    }

    /// Recovers a denied `EXECUTE` decision from protected storage.
    pub const fn recover_execute_denied(
        session_principal: PrincipalId,
        target: InvocationTarget,
        reason: ExecuteDenial,
    ) -> Self {
        Self(SecurityAuditDecisionShape::ExecuteDenied {
            session_principal,
            target,
            reason,
        })
    }

    /// Returns the closed event kind.
    pub const fn kind(&self) -> SecurityAuditKind {
        match self.0 {
            SecurityAuditDecisionShape::AuthenticationAllowed { .. }
            | SecurityAuditDecisionShape::AuthenticationDenied { .. } => {
                SecurityAuditKind::Authentication
            }
            SecurityAuditDecisionShape::ExecuteAllowed { .. }
            | SecurityAuditDecisionShape::ExecuteDenied { .. } => SecurityAuditKind::Execute,
        }
    }

    /// Returns whether the decision allowed or denied the operation.
    pub const fn outcome(&self) -> SecurityAuditOutcome {
        match self.0 {
            SecurityAuditDecisionShape::AuthenticationAllowed { .. }
            | SecurityAuditDecisionShape::ExecuteAllowed { .. } => SecurityAuditOutcome::Allowed,
            SecurityAuditDecisionShape::AuthenticationDenied { .. }
            | SecurityAuditDecisionShape::ExecuteDenied { .. } => SecurityAuditOutcome::Denied,
        }
    }

    /// Returns the authenticated or mapped principal when it is known.
    pub const fn session_principal(&self) -> Option<PrincipalId> {
        match self.0 {
            SecurityAuditDecisionShape::AuthenticationAllowed { session_principal }
            | SecurityAuditDecisionShape::ExecuteAllowed {
                session_principal, ..
            }
            | SecurityAuditDecisionShape::ExecuteDenied {
                session_principal, ..
            } => Some(session_principal),
            SecurityAuditDecisionShape::AuthenticationDenied {
                session_principal, ..
            } => session_principal,
        }
    }

    /// Returns the effective principal for an allowed `EXECUTE` decision.
    pub const fn effective_principal(&self) -> Option<PrincipalId> {
        match self.0 {
            SecurityAuditDecisionShape::ExecuteAllowed {
                effective_principal,
                ..
            } => Some(effective_principal),
            _ => None,
        }
    }

    /// Returns the authorising principal for an allowed `EXECUTE` decision.
    pub const fn authorising_principal(&self) -> Option<PrincipalId> {
        match self.0 {
            SecurityAuditDecisionShape::ExecuteAllowed {
                authorising_principal,
                ..
            } => Some(authorising_principal),
            _ => None,
        }
    }

    /// Returns the pinned function target for an `EXECUTE` decision.
    pub const fn target(&self) -> Option<InvocationTarget> {
        match self.0 {
            SecurityAuditDecisionShape::ExecuteAllowed { target, .. }
            | SecurityAuditDecisionShape::ExecuteDenied { target, .. } => Some(target),
            _ => None,
        }
    }

    /// Returns the closed denial reason when the decision was denied.
    pub const fn denial(&self) -> Option<SecurityAuditDenial> {
        match self.0 {
            SecurityAuditDecisionShape::AuthenticationDenied { reason, .. } => {
                Some(SecurityAuditDenial::Authentication(reason))
            }
            SecurityAuditDecisionShape::ExecuteDenied { reason, .. } => {
                Some(SecurityAuditDenial::Execute(reason))
            }
            _ => None,
        }
    }
}

/// An invalid protected security audit decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityAuditDecisionError {
    /// Authentication principal evidence does not match its typed denial.
    AuthenticationPrincipalShape,
}

impl fmt::Display for SecurityAuditDecisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("security audit authentication principal shape is invalid")
    }
}

impl Error for SecurityAuditDecisionError {}

/// One recovered protected security audit record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityAuditEvent {
    id: SecurityAuditEventId,
    sequence: i64,
    recorded_at: SystemTime,
    decision: SecurityAuditDecision,
}

impl SecurityAuditEvent {
    /// Recovers one audit record from its exact durable facts.
    pub const fn new(
        id: SecurityAuditEventId,
        sequence: i64,
        recorded_at: SystemTime,
        decision: SecurityAuditDecision,
    ) -> Self {
        Self {
            id,
            sequence,
            recorded_at,
            decision,
        }
    }

    /// Returns the stable opaque event identity.
    pub const fn id(&self) -> SecurityAuditEventId {
        self.id
    }

    /// Returns the database ordering sequence.
    pub const fn sequence(&self) -> i64 {
        self.sequence
    }

    /// Returns the database recording time.
    pub const fn recorded_at(&self) -> SystemTime {
        self.recorded_at
    }

    /// Returns the immutable protected decision evidence.
    pub const fn decision(&self) -> &SecurityAuditDecision {
        &self.decision
    }
}

/// An immutable, validated view of security and function identities.
#[derive(Clone, Debug)]
pub struct SecuritySnapshot {
    revision: RevisionPair,
    functions: BTreeSet<FunctionId>,
    principals: BTreeMap<PrincipalId, Principal>,
    memberships: Vec<RoleMembership>,
    grants: BTreeSet<ExecuteGrant>,
    local_peer_credentials: BTreeMap<u32, LocalPeerCredential>,
}

fn role_graph_has_cycle(
    memberships: &[RoleMembership],
    principals: &BTreeMap<PrincipalId, Principal>,
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
        Self::new_with_local_peer_credentials(
            revision,
            functions,
            principals,
            memberships,
            grants,
            vec![],
        )
    }

    /// Creates a security snapshot with protected local peer credentials.
    pub fn new_with_local_peer_credentials(
        revision: RevisionPair,
        functions: Vec<FunctionId>,
        principals: Vec<Principal>,
        memberships: Vec<RoleMembership>,
        grants: Vec<ExecuteGrant>,
        local_peer_credentials: Vec<LocalPeerCredential>,
    ) -> Result<Self, SecuritySnapshotError> {
        let mut known_functions = BTreeSet::new();
        for function in functions {
            if !known_functions.insert(function) {
                return Err(SecuritySnapshotError::DuplicateFunction);
            }
        }

        let mut principals_by_id = BTreeMap::new();
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

        let mut validated_grants = BTreeSet::new();
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

        let mut local_peers_by_uid = BTreeMap::new();
        let mut local_peer_principals = BTreeSet::new();
        for credential in local_peer_credentials {
            if local_peers_by_uid
                .insert(credential.uid, credential)
                .is_some()
            {
                return Err(SecuritySnapshotError::DuplicateLocalPeerUid);
            }
            if !local_peer_principals.insert(credential.principal) {
                return Err(SecuritySnapshotError::DuplicateLocalPeerPrincipal);
            }
            if !principals_by_id.contains_key(&credential.principal) {
                return Err(SecuritySnapshotError::UnknownLocalPeerPrincipal);
            }
        }

        Ok(Self {
            revision,
            functions: known_functions,
            principals: principals_by_id,
            memberships: validated_memberships,
            grants: validated_grants,
            local_peer_credentials: local_peers_by_uid,
        })
    }

    /// Returns the active revision pair that this snapshot authorises.
    pub const fn revision(&self) -> RevisionPair {
        self.revision
    }

    /// Iterates over known functions in canonical identity order.
    pub fn functions(&self) -> impl Iterator<Item = FunctionId> + '_ {
        self.functions.iter().copied()
    }

    /// Iterates over principals in canonical identity order.
    pub fn principals(&self) -> impl Iterator<Item = Principal> + '_ {
        self.principals.values().copied()
    }

    /// Iterates over membership edges ordered by member and then role.
    pub fn memberships(&self) -> impl Iterator<Item = RoleMembership> + '_ {
        self.memberships.iter().copied()
    }

    /// Iterates over `EXECUTE` grants ordered by grantee and function.
    pub fn execute_grants(&self) -> impl Iterator<Item = ExecuteGrant> + '_ {
        self.grants.iter().copied()
    }

    /// Iterates over local peer credentials in numeric UID order.
    pub fn local_peer_credentials(&self) -> impl Iterator<Item = LocalPeerCredential> + '_ {
        self.local_peer_credentials.values().copied()
    }

    /// Authenticates a kernel-supplied Linux peer UID with no selected roles.
    pub fn authenticate_local_peer(
        &self,
        uid: u32,
    ) -> Result<AuthenticatedSession, LocalPeerAuthenticationError> {
        let credential = self
            .local_peer_credentials
            .get(&uid)
            .ok_or(LocalPeerAuthenticationError::UnknownUid)?;
        self.bind_authenticated_session(credential.principal, vec![])
            .map_err(LocalPeerAuthenticationError::InvalidPrincipal)
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
        let ExecuteDecision::Allowed(authorised) = snapshot.authorise_execute(&session, target)
        else {
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

        let denied = SecurityAuditDecision::execute_denied(
            &session,
            target,
            ExecuteDenial::MissingExecuteGrant,
        );
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
    fn audit_events_preserve_exact_signed_order_and_recording_time() {
        use std::time::{Duration, UNIX_EPOCH};

        let decision = SecurityAuditDecision::authentication_denied(
            None,
            LocalPeerAuthenticationError::UnknownUid,
        )
        .expect("valid unknown-peer audit decision");
        let id = SecurityAuditEventId::from_bytes([0x42; 16]);
        let recorded_at = UNIX_EPOCH - Duration::from_secs(1);
        let event = SecurityAuditEvent::new(id, -7, recorded_at, decision);

        assert_eq!(event.id(), id);
        assert_eq!(event.sequence(), -7);
        assert_eq!(event.recorded_at(), recorded_at);
        assert_eq!(event.decision(), &decision);
    }
}
