//! Deny-by-default decisions for authenticated function execution.

#![deny(missing_docs)]

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    error::Error,
    fmt,
    time::SystemTime,
};

use sha2::{Digest, Sha256};

use crate::{
    FunctionId, FunctionRevisionId, InvocationId, PrincipalId, SecurityAuditEventId,
    StandardLibraryRevisionId,
    inspect::InspectPrivilege,
    revision::{RevisionPair, Sha256Digest},
    system::{SYS_INVOKE_FUNCTION_ID, system_function_by_id},
};

pub use crate::system::{CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_FUNCTION_NAME};

/// The stable principal reserved for installed catalogue-health recovery.
pub const CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID: PrincipalId =
    PrincipalId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

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

    /// Creates a principal record after rejecting the empty identity.
    ///
    /// Principal::new remains available for assembling untrusted recovered
    /// records; SecuritySnapshot validates those records before they can
    /// participate in a security decision. New callers at an input boundary
    /// should prefer this checked constructor.
    pub fn try_new(
        id: PrincipalId,
        kind: PrincipalKind,
        status: PrincipalStatus,
    ) -> Result<Self, SecuritySnapshotError> {
        if id == PrincipalId::from_bytes([0; 16]) {
            return Err(SecuritySnapshotError::EmptyPrincipal);
        }
        Ok(Self { id, kind, status })
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

/// The closed class of one invocation target function.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TargetClass {
    /// A function in the pinned application catalogue.
    Application,
    /// A function in the exact verified standard snapshot pinned by the
    /// application revision that owns the deciding security snapshot.
    VerifiedStandard,
}

/// One function in the closed two-class `EXECUTE` target union.
///
/// The canonical security snapshot is the identity-ordered union of the
/// pinned application catalogue functions and the functions of the exact
/// verified standard snapshot pinned by that application revision. Every
/// member carries its closed class. A verified-standard member also carries
/// the exact immutable standard snapshot revision and executable function
/// revision that pin it; a current, different, or unverified standard
/// snapshot cannot authorise it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SecurityFunctionTarget {
    function: FunctionId,
    class: TargetClass,
    standard_revision: Option<StandardLibraryRevisionId>,
    executable_revision: Option<FunctionRevisionId>,
}

impl SecurityFunctionTarget {
    /// Creates one application-catalogue function target.
    pub const fn application(function: FunctionId) -> Self {
        Self {
            function,
            class: TargetClass::Application,
            standard_revision: None,
            executable_revision: None,
        }
    }

    /// Creates one verified-standard function target pinned to one immutable
    /// standard snapshot and one immutable executable function revision.
    pub const fn verified_standard(
        function: FunctionId,
        standard_revision: StandardLibraryRevisionId,
        executable_revision: FunctionRevisionId,
    ) -> Self {
        Self {
            function,
            class: TargetClass::VerifiedStandard,
            standard_revision: Some(standard_revision),
            executable_revision: Some(executable_revision),
        }
    }

    /// Returns the stable function identity.
    pub const fn function(self) -> FunctionId {
        self.function
    }

    /// Returns the closed target class.
    pub const fn class(self) -> TargetClass {
        self.class
    }

    /// Returns the exact standard snapshot revision that pins this target.
    pub const fn standard_revision(self) -> Option<StandardLibraryRevisionId> {
        self.standard_revision
    }

    /// Returns the exact pinned executable function revision.
    pub const fn executable_revision(self) -> Option<FunctionRevisionId> {
        self.executable_revision
    }
}

/// A function and revision pair selected by the server for one invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvocationTarget {
    function: FunctionId,
    revision: RevisionPair,
    class: Option<TargetClass>,
    standard_revision: Option<StandardLibraryRevisionId>,
    executable_revision: Option<FunctionRevisionId>,
}

impl InvocationTarget {
    /// Creates a pinned application function target.
    ///
    /// A class-less target names only an application-catalogue function. It
    /// never authorises a verified-standard function; the protected
    /// `sys.invoke` boundary adds the class and immutable pins for standard
    /// targets. The raw dispatcher therefore remains closed to every standard
    /// target even when a grant exists for the protected gateway.
    pub const fn new(function: FunctionId, revision: RevisionPair) -> Self {
        Self {
            function,
            revision,
            class: None,
            standard_revision: None,
            executable_revision: None,
        }
    }

    /// Pins one target to one immutable executable in the exact verified
    /// standard snapshot selected by the application revision pair.
    pub const fn verified_standard(
        function: FunctionId,
        revision: RevisionPair,
        standard_revision: StandardLibraryRevisionId,
        executable_revision: FunctionRevisionId,
    ) -> Self {
        Self {
            function,
            revision,
            class: Some(TargetClass::VerifiedStandard),
            standard_revision: Some(standard_revision),
            executable_revision: Some(executable_revision),
        }
    }

    /// Returns the selected function identity.
    pub const fn function(self) -> FunctionId {
        self.function
    }

    /// Returns the exact pinned revision pair.
    pub const fn revision(self) -> RevisionPair {
        self.revision
    }

    /// Returns the closed target class when the caller pinned one.
    pub const fn class(self) -> Option<TargetClass> {
        self.class
    }

    /// Returns the exact verified standard snapshot revision pin, when present.
    pub const fn standard_revision(self) -> Option<StandardLibraryRevisionId> {
        self.standard_revision
    }

    /// Returns the exact pinned executable function revision, when present.
    pub const fn executable_revision(self) -> Option<FunctionRevisionId> {
        self.executable_revision
    }
}

/// An opaque binding for one authenticated session instance.
///
/// Clones of an authenticated session retain this binding, while separately
/// authenticated sessions receive distinct bindings. No principal identity is
/// carried by this value.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct AuthenticatedSessionBinding(InvocationId);

impl fmt::Debug for AuthenticatedSessionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthenticatedSessionBinding(..)")
    }
}

impl AuthenticatedSessionBinding {
    fn new() -> Self {
        Self(InvocationId::new())
    }
}

/// A session identity bound from trusted authentication state.
#[derive(Clone, Debug)]
pub struct AuthenticatedSession {
    principal: PrincipalId,
    active_roles: Vec<PrincipalId>,
    binding: AuthenticatedSessionBinding,
}

impl PartialEq for AuthenticatedSession {
    fn eq(&self, other: &Self) -> bool {
        self.principal == other.principal && self.active_roles == other.active_roles
    }
}

impl Eq for AuthenticatedSession {}

impl AuthenticatedSession {
    /// Returns the opaque identity shared by clones of this authenticated
    /// session. The binding carries no principal identity.
    pub const fn binding(&self) -> AuthenticatedSessionBinding {
        self.binding
    }

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
    /// A principal record uses the empty identity.
    EmptyPrincipal,
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
    /// Two privilege grants name the same grantee, class, and object.
    DuplicatePrivilegeGrant,
    /// A privilege grant names a principal absent from the snapshot.
    UnknownPrivilegeGrantPrincipal,
    /// An object-scoped privilege grant names a function absent from the snapshot.
    UnknownPrivilegeGrantObject,
}

impl fmt::Display for SecuritySnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyPrincipal => "security snapshot contains an empty principal identity",
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
            Self::DuplicatePrivilegeGrant => {
                "security snapshot contains a duplicate privilege grant"
            }
            Self::UnknownPrivilegeGrantPrincipal => {
                "security snapshot privilege grant has an unknown principal"
            }
            Self::UnknownPrivilegeGrantObject => {
                "security snapshot privilege grant has an unknown object"
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
    security_context_digest: Sha256Digest,
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

    /// Returns the immutable canonical digest of the validated security snapshot
    /// that authorised this invocation.
    ///
    /// The digest is local decision evidence only; it is not a transport or
    /// audit field.
    pub const fn security_context_digest(&self) -> Sha256Digest {
        self.security_context_digest
    }
}

/// The reason a function invocation was denied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecuteDenial {
    /// The session was not valid in this snapshot.
    InvalidSession,
    /// The snapshot does not contain the requested function, the target class
    /// does not match the union member, or a verified-standard target is not
    /// pinned to the exact verified standard snapshot.
    UnknownFunction,
    /// The requested revision pair is not the snapshot's active pair.
    RevisionMismatch,
    /// No direct or selected-role grant authorises the function.
    MissingExecuteGrant,
    /// The target uses `SECURITY DEFINER`, which this protected invocation
    /// boundary cannot execute with its required owner transition semantics.
    UnsupportedSecurityDefiner,
}

/// The complete result of an `EXECUTE` authorisation check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecuteDecision {
    /// Execution is allowed with immutable decision evidence.
    Allowed(AuthorisedInvocation),
    /// Execution is denied without entering an evaluator or executor.
    Denied(ExecuteDenial),
}

/// The closed ownership classification of one inspection epoch.
///
/// The classification drives the INSPECT ladder: an own epoch needs the
/// `OwnInvocation` rung, a session-scoped epoch needs `SessionInvocations`,
/// and a foreign epoch needs `AnyInvocation`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectEpochScope {
    /// The epoch belongs to the session principal itself.
    Own,
    /// The epoch is session-scoped with no single owning principal.
    Session,
    /// The epoch belongs to another principal.
    Foreign,
}

/// The closed reason an INSPECT decision was denied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectDenial {
    /// The session principal holds no granted privilege that reaches the
    /// epoch's required scope or the requested classification dimension.
    MissingPrivilege,
    /// The requested epoch does not exist.
    MissingEpoch,
    /// The trace is the inspecting invocation's own observation and
    /// self-observation is suppressed (spec docs/31).
    ObserverSuppressed,
}

impl InspectDenial {
    /// Returns the stable closed audit reason recorded for this denial.
    pub const fn audit_reason(self) -> &'static str {
        match self {
            Self::MissingPrivilege => "inspect:missing-privilege",
            Self::MissingEpoch => "inspect:missing-epoch",
            Self::ObserverSuppressed => "inspect:observer-suppressed",
        }
    }
}

/// The complete result of one INSPECT authorisation check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectDecision {
    /// Inspection is allowed with the epoch access facts.
    Allowed {
        /// The ownership classification the ladder admitted.
        epoch_scope: InspectEpochScope,
        /// The privilege that was requested and granted for this epoch.
        requested: InspectPrivilege,
    },
    /// Inspection is denied with a closed reason.
    Denied(InspectDenial),
}

/// Decides whether one session principal may apply one INSPECT privilege to
/// one inspection epoch.
///
/// The invocation-scope ladder is closed: `OwnInvocation` reaches only epochs
/// owned by the session principal, `SessionInvocations` reaches own and
/// session-scoped epochs, and `AnyInvocation` reaches every epoch. The four
/// content classifiers (`Values`, `Source`, `SecurityDetails`,
/// `RuntimeInternals`) are orthogonal: each grants exactly its own redaction
/// dimension and never a ladder rung, and a ladder rung never grants a
/// classification dimension.
///
/// `epoch_owner` is the principal that owns the epoch; `None` denotes a
/// session-scoped epoch with no single owning principal (service or
/// session-level records). An epoch that does not exist at all is denied by
/// the kernel with [`InspectDenial::MissingEpoch`] before this ladder runs,
/// and self-observation suppression uses [`InspectDenial::ObserverSuppressed`]
/// at the trace boundary; both share this closed denial set.
///
/// The decision fails closed: a request that the granted set does not cover —
/// either a ladder rung below the epoch's required scope or the requested
/// scope, or a classifier that is not granted — is denied with
/// [`InspectDenial::MissingPrivilege`] without exposing any epoch content.
pub fn authorise_inspect(
    session_principal: PrincipalId,
    requested: InspectPrivilege,
    epoch_owner: Option<PrincipalId>,
    granted: &[InspectPrivilege],
) -> InspectDecision {
    let (required_rung, epoch_scope) = match epoch_owner {
        Some(owner) if owner == session_principal => (0, InspectEpochScope::Own),
        Some(_) => (2, InspectEpochScope::Foreign),
        None => (1, InspectEpochScope::Session),
    };
    let Some(granted_rung) = granted
        .iter()
        .filter_map(|privilege| privilege.ladder_rank())
        .max()
    else {
        return InspectDecision::Denied(InspectDenial::MissingPrivilege);
    };
    let requested_rung = requested.ladder_rank().unwrap_or(0);
    if granted_rung < required_rung || granted_rung < requested_rung {
        return InspectDecision::Denied(InspectDenial::MissingPrivilege);
    }
    if let Some(classifier) = requested.classifier()
        && !granted
            .iter()
            .any(|privilege| privilege.classifier() == Some(classifier))
    {
        return InspectDecision::Denied(InspectDenial::MissingPrivilege);
    }
    InspectDecision::Allowed {
        epoch_scope,
        requested,
    }
}

/// The closed privilege-class set of one privilege grant.
///
/// `Execute` and `SecurityAdmin` are class-wide privileges. `Inspect`
/// carries exactly one closed INSPECT sub-privilege; there is no wildcard
/// INSPECT class.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PrivilegeClass {
    /// The privilege to execute a function, or any function class-wide.
    Execute,
    /// One closed INSPECT privilege from the sealed INSPECT set.
    Inspect(InspectPrivilege),
    /// The protected security-administration privilege.
    SecurityAdmin,
}

impl fmt::Display for PrivilegeClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Execute => "execute",
            Self::SecurityAdmin => "security_admin",
            Self::Inspect(privilege) => match privilege {
                InspectPrivilege::OwnInvocation => "inspect:own-invocation",
                InspectPrivilege::SessionInvocations => "inspect:session-invocations",
                InspectPrivilege::AnyInvocation => "inspect:any-invocation",
                InspectPrivilege::Values => "inspect:values",
                InspectPrivilege::Source => "inspect:source",
                InspectPrivilege::SecurityDetails => "inspect:security-details",
                InspectPrivilege::RuntimeInternals => "inspect:runtime-internals",
            },
        })
    }
}

/// An invalid privilege grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivilegeGrantError {
    /// The grantee is the empty identity.
    EmptyGrantee,
    /// The object names the empty function identity.
    EmptyObject,
    /// A `SecurityAdmin` grant is object-scoped instead of class-wide.
    SecurityAdminObject,
}

impl fmt::Display for PrivilegeGrantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyGrantee => "privilege grant has an empty grantee identity",
            Self::EmptyObject => "privilege grant has an empty object identity",
            Self::SecurityAdminObject => "security_admin privilege grant must be class-wide",
        })
    }
}

impl Error for PrivilegeGrantError {}

/// A privilege-class grant from one grantee to one class and object.
///
/// `object` is `None` for a class-wide grant and `Some(function)` for a
/// function-scoped grant. The class is closed by construction: every
/// variant is a concrete privilege, so there is no empty class.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PrivilegeGrant {
    grantee: PrincipalId,
    class: PrivilegeClass,
    object: Option<FunctionId>,
}

impl PrivilegeGrant {
    /// Creates a privilege grant with the closed invariants checked.
    ///
    /// The grantee must not be the empty identity, and an object-scoped
    /// grant must name a non-empty function identity. `SecurityAdmin` grants
    /// are class-wide and therefore cannot name an object.
    pub fn new(
        grantee: PrincipalId,
        class: PrivilegeClass,
        object: Option<FunctionId>,
    ) -> Result<Self, PrivilegeGrantError> {
        if grantee == PrincipalId::from_bytes([0; 16]) {
            return Err(PrivilegeGrantError::EmptyGrantee);
        }
        if let Some(function) = object
            && function == FunctionId::from_bytes([0; 16])
        {
            return Err(PrivilegeGrantError::EmptyObject);
        }
        if matches!(class, PrivilegeClass::SecurityAdmin) && object.is_some() {
            return Err(PrivilegeGrantError::SecurityAdminObject);
        }
        Ok(Self {
            grantee,
            class,
            object,
        })
    }

    /// Returns the principal that received the grant.
    pub const fn grantee(self) -> PrincipalId {
        self.grantee
    }

    /// Returns the closed privilege class of the grant.
    pub const fn class(self) -> PrivilegeClass {
        self.class
    }

    /// Returns the object when the grant is function-scoped.
    pub const fn object(self) -> Option<FunctionId> {
        self.object
    }

    /// Returns whether this grant applies to the whole class.
    pub const fn is_class_wide(self) -> bool {
        self.object.is_none()
    }
}

/// The closed reason a privilege-class decision was denied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivilegeDenial {
    /// The requested privilege class is not granted for the object.
    MissingPrivilege {
        /// The privilege class that was requested and denied.
        requested: PrivilegeClass,
    },
}

impl PrivilegeDenial {
    /// Returns the stable closed audit reason recorded for this denial.
    pub const fn audit_reason(self) -> &'static str {
        match self {
            Self::MissingPrivilege { requested } => match requested {
                PrivilegeClass::Execute => "execute:missing-privilege",
                PrivilegeClass::Inspect(_) => "inspect:missing-privilege",
                PrivilegeClass::SecurityAdmin => "security_admin:missing-privilege",
            },
        }
    }
}

/// The complete result of one privilege-class authorisation check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivilegeDecision {
    /// The requested privilege class is granted for the object.
    Allowed {
        /// The privilege class that was requested and granted.
        requested: PrivilegeClass,
    },
    /// The requested privilege class is denied with a closed reason.
    Denied(PrivilegeDenial),
}

/// Decides whether one principal holds one privilege class for one object.
///
/// `granted` is the closed set of privilege classes the principal holds for
/// `object`: a caller that resolves durable grants must first keep exactly
/// the grants whose object is `None` or equals the requested object, then
/// pass their classes here. `session_or_principal` names the principal
/// being decided and is retained as decision evidence; the decision itself
/// is closed over the granted set.
///
/// `Execute` is object-capable and `SecurityAdmin` is class-wide: a
/// `SecurityAdmin` request naming an object is denied even when the class is
/// granted. `Inspect` applies the closed INSPECT ladder: a granted higher rung reaches a
/// requested lower rung, and a requested content classifier must itself be
/// granted. The decision fails closed: a request the granted set does not
/// cover is denied with [`PrivilegeDenial::MissingPrivilege`] without
/// exposing any grant or object content beyond the requested class.
pub fn authorise_privilege(
    session_or_principal: PrincipalId,
    requested: PrivilegeClass,
    object: Option<FunctionId>,
    granted: &[PrivilegeClass],
) -> PrivilegeDecision {
    let covered = match requested {
        PrivilegeClass::Execute => granted.contains(&requested),
        PrivilegeClass::SecurityAdmin => object.is_none() && granted.contains(&requested),
        PrivilegeClass::Inspect(requested_privilege) => {
            let required_rung = requested_privilege.ladder_rank().unwrap_or(0);
            let granted_rung = granted
                .iter()
                .filter_map(|class| match class {
                    PrivilegeClass::Inspect(privilege) => privilege.ladder_rank(),
                    _ => None,
                })
                .max();
            let ladder_covered = granted_rung.is_some_and(|rung| rung >= required_rung);
            let classifier_covered = match requested_privilege.classifier() {
                None => true,
                Some(classifier) => granted.iter().any(|class| {
                    matches!(
                        class,
                        PrivilegeClass::Inspect(privilege)
                            if privilege.classifier() == Some(classifier)
                    )
                }),
            };
            ladder_covered && classifier_covered
        }
    };
    let _ = (session_or_principal, object);
    if covered {
        PrivilegeDecision::Allowed { requested }
    } else {
        PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { requested })
    }
}

/// The closed family of protected security audit events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityAuditKind {
    /// A local connection attempted to establish an authenticated session.
    Authentication,
    /// An authenticated session requested permission to execute a function.
    Execute,
    /// The local client checked a CLIENT function capability requirement.
    Capability,
    /// An authenticated session loaded or wrote durable USER state cells.
    UserState,
    /// An authenticated session requested INSPECT access to an inspection epoch.
    Inspect,
    /// An authenticated session performed or attempted a security-admin
    /// mutation.
    SecurityAdmin,
    /// An installed host applied a prepared source revision.
    SourceApply,
}

/// The closed USER state operation family recorded in protected audit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserStateAuditOperation {
    /// A `load_user_state` operation.
    Load,
    /// A `write_user_state` operation.
    Write,
}

/// The closed security-admin operation family recorded in protected audit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityAdminAuditOperation {
    /// A `sys.security.create_principal` operation.
    CreatePrincipal,
    /// A `sys.security.disable_principal` operation.
    DisablePrincipal,
    /// A `sys.security.create_role` operation.
    CreateRole,
    /// A `sys.security.grant_role` operation.
    GrantRole,
    /// A `sys.security.revoke_role` operation.
    RevokeRole,
    /// A `sys.security.grant_privilege` operation.
    GrantPrivilege,
    /// A `sys.security.revoke_privilege` operation.
    RevokePrivilege,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SecurityAuditDenial {
    /// Local peer authentication failed for the recorded reason.
    Authentication(LocalPeerAuthenticationError),
    /// Function execution was denied for the recorded reason.
    Execute(ExecuteDenial),
    /// A CLIENT capability requirement was denied. Only the redacted
    /// qualified capability name is recorded; argument values are never
    /// written to audit.
    Capability {
        /// The redacted qualified capability name (no arguments).
        capability: String,
    },
    /// An INSPECT decision was denied for the recorded closed reason.
    Inspect(InspectDenial),
    /// A security-admin operation was denied for the recorded closed reason.
    SecurityAdmin(PrivilegeDenial),
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
    CapabilityAllowed {
        session_principal: PrincipalId,
        target: InvocationTarget,
        capability: String,
    },
    CapabilityDenied {
        session_principal: PrincipalId,
        target: InvocationTarget,
        capability: String,
    },
    UserStateAllowed {
        session_principal: PrincipalId,
        operation: UserStateAuditOperation,
        root_function: FunctionId,
        cell_count: u64,
    },
    InspectAllowed {
        session_principal: PrincipalId,
        requested: InspectPrivilege,
        epoch_scope: InspectEpochScope,
        epoch_owner: Option<PrincipalId>,
    },
    InspectDenied {
        session_principal: PrincipalId,
        epoch_owner: Option<PrincipalId>,
        reason: InspectDenial,
    },
    SecurityAdminAllowed {
        session_principal: PrincipalId,
        operation: SecurityAdminAuditOperation,
        target: FunctionId,
    },
    SecurityAdminDenied {
        session_principal: PrincipalId,
        operation: SecurityAdminAuditOperation,
        target: FunctionId,
        reason: PrivilegeDenial,
    },
    SourceApplyAllowed {
        session_principal: PrincipalId,
        candidate: RevisionPair,
    },
}

/// An immutable authentication, `EXECUTE`, CLIENT capability, USER state,
/// or installed source-apply decision prepared for auditing.
#[derive(Clone, Debug, Eq, PartialEq)]
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

    /// Records an allowed CLIENT capability decision for a trusted session.
    ///
    /// Only the qualified capability name is recorded; argument values are
    /// never written to audit.
    pub fn capability_allowed(
        session: &AuthenticatedSession,
        target: InvocationTarget,
        capability: impl Into<String>,
    ) -> Result<Self, SecurityAuditDecisionError> {
        Self::recover_capability_allowed(session.principal, target, capability)
    }

    /// Recovers an allowed CLIENT capability decision from protected storage.
    pub fn recover_capability_allowed(
        session_principal: PrincipalId,
        target: InvocationTarget,
        capability: impl Into<String>,
    ) -> Result<Self, SecurityAuditDecisionError> {
        let capability = capability.into();
        validate_capability_name(&capability)?;
        Ok(Self(SecurityAuditDecisionShape::CapabilityAllowed {
            session_principal,
            target,
            capability,
        }))
    }

    /// Records a denied CLIENT capability decision for a trusted session.
    ///
    /// Only the redacted qualified capability name is recorded; argument
    /// values are never written to audit.
    pub fn capability_denied(
        session: &AuthenticatedSession,
        target: InvocationTarget,
        capability: impl Into<String>,
    ) -> Result<Self, SecurityAuditDecisionError> {
        Self::recover_capability_denied(session.principal, target, capability)
    }

    /// Recovers a denied CLIENT capability decision from protected storage.
    pub fn recover_capability_denied(
        session_principal: PrincipalId,
        target: InvocationTarget,
        capability: impl Into<String>,
    ) -> Result<Self, SecurityAuditDecisionError> {
        let capability = capability.into();
        validate_capability_name(&capability)?;
        Ok(Self(SecurityAuditDecisionShape::CapabilityDenied {
            session_principal,
            target,
            capability,
        }))
    }
    /// Records an allowed USER state operation for a trusted session.
    ///
    /// Only the operation kind, principal, root function, and cell count are
    /// retained; typed value payloads are never written to audit.
    pub fn user_state_allowed(
        session: &AuthenticatedSession,
        operation: UserStateAuditOperation,
        root_function: FunctionId,
        cell_count: u64,
    ) -> Self {
        Self::recover_user_state_allowed(session.principal, operation, root_function, cell_count)
    }

    /// Recovers an allowed USER state operation from protected storage.
    pub const fn recover_user_state_allowed(
        session_principal: PrincipalId,
        operation: UserStateAuditOperation,
        root_function: FunctionId,
        cell_count: u64,
    ) -> Self {
        Self(SecurityAuditDecisionShape::UserStateAllowed {
            session_principal,
            operation,
            root_function,
            cell_count,
        })
    }

    /// Records an allowed INSPECT decision from its epoch access facts.
    ///
    /// The decision must be an `Allowed` decision; a denied decision reaches
    /// the denied constructor instead and fails closed here.
    pub fn inspect_allowed(
        session: &AuthenticatedSession,
        decision: InspectDecision,
        epoch_owner: Option<PrincipalId>,
    ) -> Result<Self, SecurityAuditDecisionError> {
        let InspectDecision::Allowed {
            requested,
            epoch_scope,
        } = decision
        else {
            return Err(SecurityAuditDecisionError::InspectDecisionShape);
        };
        Ok(Self(SecurityAuditDecisionShape::InspectAllowed {
            session_principal: session.principal,
            requested,
            epoch_scope,
            epoch_owner,
        }))
    }

    /// Recovers an allowed INSPECT decision from protected storage.
    pub const fn recover_inspect_allowed(
        session_principal: PrincipalId,
        requested: InspectPrivilege,
        epoch_scope: InspectEpochScope,
        epoch_owner: Option<PrincipalId>,
    ) -> Self {
        Self(SecurityAuditDecisionShape::InspectAllowed {
            session_principal,
            requested,
            epoch_scope,
            epoch_owner,
        })
    }

    /// Records a denied INSPECT decision for a trusted session.
    ///
    /// Only the closed denial reason and the epoch owner are retained; no
    /// epoch content is ever written to audit.
    pub fn inspect_denied(
        session: &AuthenticatedSession,
        epoch_owner: Option<PrincipalId>,
        reason: InspectDenial,
    ) -> Self {
        Self::recover_inspect_denied(session.principal, epoch_owner, reason)
    }

    /// Recovers a denied INSPECT decision from protected storage.
    pub const fn recover_inspect_denied(
        session_principal: PrincipalId,
        epoch_owner: Option<PrincipalId>,
        reason: InspectDenial,
    ) -> Self {
        Self(SecurityAuditDecisionShape::InspectDenied {
            session_principal,
            epoch_owner,
            reason,
        })
    }

    /// Records an allowed security-admin decision from its privilege decision.
    ///
    /// The operation kind, principal, and sealed target identity are
    /// retained; argument payloads are never written to audit. The decision
    /// must be an `Allowed` decision for the `SecurityAdmin` class; any
    /// other decision fails closed here.
    pub fn security_admin_allowed(
        session: &AuthenticatedSession,
        decision: PrivilegeDecision,
        operation: SecurityAdminAuditOperation,
        target: FunctionId,
    ) -> Result<Self, SecurityAuditDecisionError> {
        let PrivilegeDecision::Allowed { requested } = decision else {
            return Err(SecurityAuditDecisionError::SecurityAdminDecisionShape);
        };
        if requested != PrivilegeClass::SecurityAdmin {
            return Err(SecurityAuditDecisionError::SecurityAdminDecisionShape);
        }
        Ok(Self::recover_security_admin_allowed(
            session.principal,
            operation,
            target,
        ))
    }

    /// Recovers an allowed security-admin decision from protected storage.
    pub const fn recover_security_admin_allowed(
        session_principal: PrincipalId,
        operation: SecurityAdminAuditOperation,
        target: FunctionId,
    ) -> Self {
        Self(SecurityAuditDecisionShape::SecurityAdminAllowed {
            session_principal,
            operation,
            target,
        })
    }

    /// Records a denied security-admin decision for a trusted session.
    ///
    /// Only the closed denial reason, operation kind, principal, and sealed
    /// target identity are retained; no argument payload is ever written to
    /// audit.
    pub fn security_admin_denied(
        session: &AuthenticatedSession,
        operation: SecurityAdminAuditOperation,
        target: FunctionId,
        reason: PrivilegeDenial,
    ) -> Self {
        Self::recover_security_admin_denied(session.principal, operation, target, reason)
    }

    /// Recovers a denied security-admin decision from protected storage.
    pub const fn recover_security_admin_denied(
        session_principal: PrincipalId,
        operation: SecurityAdminAuditOperation,
        target: FunctionId,
        reason: PrivilegeDenial,
    ) -> Self {
        Self(SecurityAuditDecisionShape::SecurityAdminDenied {
            session_principal,
            operation,
            target,
            reason,
        })
    }

    /// Records an allowed installed source apply from a trusted session.
    pub fn source_apply_allowed(session: &AuthenticatedSession, candidate: RevisionPair) -> Self {
        Self::recover_source_apply_allowed(session.principal, candidate)
    }

    /// Recovers an allowed installed source apply from protected storage.
    pub const fn recover_source_apply_allowed(
        session_principal: PrincipalId,
        candidate: RevisionPair,
    ) -> Self {
        Self(SecurityAuditDecisionShape::SourceApplyAllowed {
            session_principal,
            candidate,
        })
    }

    /// Returns the USER state operation when this decision records one.
    pub const fn user_state_operation(&self) -> Option<UserStateAuditOperation> {
        match self.0 {
            SecurityAuditDecisionShape::UserStateAllowed { operation, .. } => Some(operation),
            _ => None,
        }
    }

    /// Returns the root function when this decision records USER state.
    pub const fn user_state_root_function(&self) -> Option<FunctionId> {
        match self.0 {
            SecurityAuditDecisionShape::UserStateAllowed { root_function, .. } => {
                Some(root_function)
            }
            _ => None,
        }
    }

    /// Returns the cell count when this decision records USER state.
    pub const fn user_state_cell_count(&self) -> Option<u64> {
        match self.0 {
            SecurityAuditDecisionShape::UserStateAllowed { cell_count, .. } => Some(cell_count),
            _ => None,
        }
    }

    /// Returns the requested privilege when this decision records INSPECT.
    pub const fn inspect_requested(&self) -> Option<InspectPrivilege> {
        match self.0 {
            SecurityAuditDecisionShape::InspectAllowed { requested, .. } => Some(requested),
            _ => None,
        }
    }

    /// Returns the epoch scope when this decision records INSPECT.
    pub const fn inspect_epoch_scope(&self) -> Option<InspectEpochScope> {
        match self.0 {
            SecurityAuditDecisionShape::InspectAllowed { epoch_scope, .. } => Some(epoch_scope),
            _ => None,
        }
    }

    /// Returns the closed denial reason when this decision records a denied
    /// INSPECT decision.
    pub const fn inspect_denial(&self) -> Option<InspectDenial> {
        match self.0 {
            SecurityAuditDecisionShape::InspectDenied { reason, .. } => Some(reason),
            _ => None,
        }
    }

    /// Returns the operation kind when this decision records security admin.
    pub const fn security_admin_operation(&self) -> Option<SecurityAdminAuditOperation> {
        match self.0 {
            SecurityAuditDecisionShape::SecurityAdminAllowed { operation, .. }
            | SecurityAuditDecisionShape::SecurityAdminDenied { operation, .. } => Some(operation),
            _ => None,
        }
    }

    /// Returns the candidate revision when this decision records an installed
    /// source apply.
    pub const fn source_apply_candidate(&self) -> Option<RevisionPair> {
        match self.0 {
            SecurityAuditDecisionShape::SourceApplyAllowed { candidate, .. } => Some(candidate),
            _ => None,
        }
    }

    /// Returns the sealed target identity when this decision records
    /// security admin.
    pub const fn security_admin_target(&self) -> Option<FunctionId> {
        match self.0 {
            SecurityAuditDecisionShape::SecurityAdminAllowed { target, .. }
            | SecurityAuditDecisionShape::SecurityAdminDenied { target, .. } => Some(target),
            _ => None,
        }
    }

    /// Returns the closed denial reason when this decision records a denied
    /// security-admin decision.
    pub const fn security_admin_denial(&self) -> Option<PrivilegeDenial> {
        match self.0 {
            SecurityAuditDecisionShape::SecurityAdminDenied { reason, .. } => Some(reason),
            _ => None,
        }
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
            SecurityAuditDecisionShape::CapabilityAllowed { .. }
            | SecurityAuditDecisionShape::CapabilityDenied { .. } => SecurityAuditKind::Capability,
            SecurityAuditDecisionShape::UserStateAllowed { .. } => SecurityAuditKind::UserState,
            SecurityAuditDecisionShape::InspectAllowed { .. }
            | SecurityAuditDecisionShape::InspectDenied { .. } => SecurityAuditKind::Inspect,
            SecurityAuditDecisionShape::SecurityAdminAllowed { .. }
            | SecurityAuditDecisionShape::SecurityAdminDenied { .. } => {
                SecurityAuditKind::SecurityAdmin
            }
            SecurityAuditDecisionShape::SourceApplyAllowed { .. } => SecurityAuditKind::SourceApply,
        }
    }

    /// Returns whether the decision allowed or denied the operation.
    pub const fn outcome(&self) -> SecurityAuditOutcome {
        match self.0 {
            SecurityAuditDecisionShape::AuthenticationAllowed { .. }
            | SecurityAuditDecisionShape::ExecuteAllowed { .. }
            | SecurityAuditDecisionShape::CapabilityAllowed { .. }
            | SecurityAuditDecisionShape::UserStateAllowed { .. }
            | SecurityAuditDecisionShape::InspectAllowed { .. }
            | SecurityAuditDecisionShape::SecurityAdminAllowed { .. }
            | SecurityAuditDecisionShape::SourceApplyAllowed { .. } => {
                SecurityAuditOutcome::Allowed
            }
            SecurityAuditDecisionShape::AuthenticationDenied { .. }
            | SecurityAuditDecisionShape::ExecuteDenied { .. }
            | SecurityAuditDecisionShape::CapabilityDenied { .. }
            | SecurityAuditDecisionShape::InspectDenied { .. }
            | SecurityAuditDecisionShape::SecurityAdminDenied { .. } => {
                SecurityAuditOutcome::Denied
            }
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
            }
            | SecurityAuditDecisionShape::CapabilityAllowed {
                session_principal, ..
            }
            | SecurityAuditDecisionShape::CapabilityDenied {
                session_principal, ..
            }
            | SecurityAuditDecisionShape::UserStateAllowed {
                session_principal, ..
            }
            | SecurityAuditDecisionShape::InspectAllowed {
                session_principal, ..
            }
            | SecurityAuditDecisionShape::InspectDenied {
                session_principal, ..
            }
            | SecurityAuditDecisionShape::SecurityAdminAllowed {
                session_principal, ..
            }
            | SecurityAuditDecisionShape::SecurityAdminDenied {
                session_principal, ..
            }
            | SecurityAuditDecisionShape::SourceApplyAllowed {
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
            | SecurityAuditDecisionShape::ExecuteDenied { target, .. }
            | SecurityAuditDecisionShape::CapabilityAllowed { target, .. }
            | SecurityAuditDecisionShape::CapabilityDenied { target, .. } => Some(target),
            _ => None,
        }
    }

    /// Returns the redacted qualified capability name of a CLIENT capability
    /// decision.
    ///
    /// Argument values are never part of the recorded name.
    pub fn capability_name(&self) -> Option<&str> {
        match &self.0 {
            SecurityAuditDecisionShape::CapabilityAllowed { capability, .. }
            | SecurityAuditDecisionShape::CapabilityDenied { capability, .. } => Some(capability),
            _ => None,
        }
    }

    /// Returns the closed denial reason when the decision was denied.
    pub fn denial(&self) -> Option<SecurityAuditDenial> {
        match &self.0 {
            SecurityAuditDecisionShape::AuthenticationDenied { reason, .. } => {
                Some(SecurityAuditDenial::Authentication(*reason))
            }
            SecurityAuditDecisionShape::ExecuteDenied { reason, .. } => {
                Some(SecurityAuditDenial::Execute(*reason))
            }
            SecurityAuditDecisionShape::CapabilityDenied { capability, .. } => {
                Some(SecurityAuditDenial::Capability {
                    capability: capability.clone(),
                })
            }
            SecurityAuditDecisionShape::InspectDenied { reason, .. } => {
                Some(SecurityAuditDenial::Inspect(*reason))
            }
            SecurityAuditDecisionShape::SecurityAdminDenied { reason, .. } => {
                Some(SecurityAuditDenial::SecurityAdmin(*reason))
            }
            _ => None,
        }
    }
}

/// Validates that an audit capability name is a qualified name with no
/// arguments: at least two dot-separated lowercase identifier segments.
///
/// Paths, hosts, secret ids, and any argument form fail this closed shape, so
/// no disclosure-bearing value can ever be recorded as a capability name.
fn validate_capability_name(name: &str) -> Result<(), SecurityAuditDecisionError> {
    let segments = name.split('.');
    let mut segment_count = 0usize;
    for segment in segments {
        segment_count += 1;
        let mut characters = segment.chars();
        let Some(first) = characters.next() else {
            return Err(SecurityAuditDecisionError::CapabilityNameShape);
        };
        if !first.is_ascii_lowercase() {
            return Err(SecurityAuditDecisionError::CapabilityNameShape);
        }
        if !characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        }) {
            return Err(SecurityAuditDecisionError::CapabilityNameShape);
        }
    }
    if segment_count < 2 {
        return Err(SecurityAuditDecisionError::CapabilityNameShape);
    }
    Ok(())
}

/// An invalid protected security audit decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityAuditDecisionError {
    /// Authentication principal evidence does not match its typed denial.
    AuthenticationPrincipalShape,
    /// The capability name is not a closed qualified name with no arguments.
    CapabilityNameShape,
    /// A non-allowed INSPECT decision reached an allowed-only constructor.
    InspectDecisionShape,
    /// A non-allowed or non-`SecurityAdmin` decision reached an
    /// allowed-only constructor.
    SecurityAdminDecisionShape,
}

impl fmt::Display for SecurityAuditDecisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AuthenticationPrincipalShape => {
                "security audit authentication principal shape is invalid"
            }
            Self::CapabilityNameShape => {
                "security audit capability name must be a qualified name with no arguments"
            }
            Self::InspectDecisionShape => {
                "security audit INSPECT decision must be an allowed decision"
            }
            Self::SecurityAdminDecisionShape => {
                "security audit security-admin decision must be an allowed SecurityAdmin decision"
            }
        })
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

const SECURITY_CONTEXT_DIGEST_DOMAIN: &[u8] = b"ORNA-SECURITY-SNAPSHOT-DIGEST\0\x01";

struct SecuritySnapshotDigestEncoder {
    hasher: Sha256,
}

impl SecuritySnapshotDigestEncoder {
    fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(SECURITY_CONTEXT_DIGEST_DOMAIN);
        Self { hasher }
    }

    fn field(&mut self, value: &[u8]) {
        self.hasher.update((value.len() as u64).to_be_bytes());
        self.hasher.update(value);
    }

    fn label(&mut self, value: &[u8]) {
        self.field(value);
    }

    fn count(&mut self, value: usize) {
        self.field(&(value as u64).to_be_bytes());
    }

    fn optional_bytes_16(&mut self, value: Option<[u8; 16]>) {
        match value {
            Some(value) => {
                self.field(&[1]);
                self.field(&value);
            }
            None => self.field(&[0]),
        }
    }

    fn finish(self) -> Sha256Digest {
        Sha256Digest::from_bytes(self.hasher.finalize().into())
    }
}

fn principal_kind_discriminator(kind: PrincipalKind) -> u8 {
    match kind {
        PrincipalKind::User => 1,
        PrincipalKind::Role => 2,
        PrincipalKind::Service => 3,
    }
}

fn principal_status_discriminator(status: PrincipalStatus) -> u8 {
    match status {
        PrincipalStatus::Active => 1,
        PrincipalStatus::Disabled => 2,
    }
}

fn target_class_discriminator(class: TargetClass) -> u8 {
    match class {
        TargetClass::Application => 1,
        TargetClass::VerifiedStandard => 2,
    }
}

fn privilege_class_discriminator(class: PrivilegeClass) -> u8 {
    match class {
        PrivilegeClass::Execute => 1,
        PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation) => 2,
        PrivilegeClass::Inspect(InspectPrivilege::SessionInvocations) => 3,
        PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation) => 4,
        PrivilegeClass::Inspect(InspectPrivilege::Values) => 5,
        PrivilegeClass::Inspect(InspectPrivilege::Source) => 6,
        PrivilegeClass::Inspect(InspectPrivilege::SecurityDetails) => 7,
        PrivilegeClass::Inspect(InspectPrivilege::RuntimeInternals) => 8,
        PrivilegeClass::SecurityAdmin => 9,
    }
}

fn security_snapshot_digest(
    revision: RevisionPair,
    function_targets: &BTreeMap<FunctionId, SecurityFunctionTarget>,
    principals: &BTreeMap<PrincipalId, Principal>,
    memberships: &[RoleMembership],
    grants: &BTreeSet<ExecuteGrant>,
    privilege_grants: &BTreeSet<PrivilegeGrant>,
    local_peer_credentials: &BTreeMap<u32, LocalPeerCredential>,
) -> Sha256Digest {
    let mut encoder = SecuritySnapshotDigestEncoder::new();

    encoder.label(b"revision");
    encoder.field(&revision.source().to_bytes());
    encoder.field(&revision.catalogue().to_bytes());

    encoder.label(b"function_targets");
    encoder.count(function_targets.len());
    for target in function_targets.values() {
        encoder.field(&target.function().to_bytes());
        encoder.field(&[target_class_discriminator(target.class())]);
        encoder.optional_bytes_16(
            target
                .standard_revision()
                .map(StandardLibraryRevisionId::to_bytes),
        );
        encoder.optional_bytes_16(
            target
                .executable_revision()
                .map(FunctionRevisionId::to_bytes),
        );
    }

    encoder.label(b"principals");
    encoder.count(principals.len());
    for principal in principals.values() {
        encoder.field(&principal.id().to_bytes());
        encoder.field(&[principal_kind_discriminator(principal.kind())]);
        encoder.field(&[principal_status_discriminator(principal.status())]);
    }

    encoder.label(b"memberships");
    encoder.count(memberships.len());
    for membership in memberships {
        encoder.field(&membership.member().to_bytes());
        encoder.field(&membership.role().to_bytes());
    }

    encoder.label(b"execute_grants");
    encoder.count(grants.len());
    for grant in grants {
        encoder.field(&grant.grantee().to_bytes());
        encoder.field(&grant.function().to_bytes());
    }

    encoder.label(b"privilege_grants");
    encoder.count(privilege_grants.len());
    for grant in privilege_grants {
        encoder.field(&grant.grantee().to_bytes());
        encoder.field(&[privilege_class_discriminator(grant.class())]);
        encoder.optional_bytes_16(grant.object().map(FunctionId::to_bytes));
    }

    encoder.label(b"local_peer_credentials");
    encoder.count(local_peer_credentials.len());
    for credential in local_peer_credentials.values() {
        encoder.field(&credential.uid().to_be_bytes());
        encoder.field(&credential.principal().to_bytes());
    }

    encoder.finish()
}

/// An immutable, validated view of security and function identities.
///
/// The known function set is the canonical, identity-ordered two-class union
/// of the pinned application catalogue functions and the exact verified
/// standard snapshot functions pinned by that application revision.
#[derive(Clone, Debug)]
pub struct SecuritySnapshot {
    revision: RevisionPair,
    function_targets: BTreeMap<FunctionId, SecurityFunctionTarget>,
    principals: BTreeMap<PrincipalId, Principal>,
    memberships: Vec<RoleMembership>,
    grants: BTreeSet<ExecuteGrant>,
    privilege_grants: BTreeSet<PrivilegeGrant>,
    local_peer_credentials: BTreeMap<u32, LocalPeerCredential>,
    security_context_digest: Sha256Digest,
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
    /// Creates a security snapshot from an application-only function set.
    ///
    /// Every listed function is an `Application` target. Use
    /// [`Self::new_with_function_targets`] when the exact verified standard
    /// snapshot pinned by the revision contributes functions to the union.
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
    ///
    /// Every listed function is an `Application` target. Use
    /// [`Self::new_with_function_targets_and_local_peer_credentials`] when the
    /// exact verified standard snapshot contributes functions to the union.
    pub fn new_with_local_peer_credentials(
        revision: RevisionPair,
        functions: Vec<FunctionId>,
        principals: Vec<Principal>,
        memberships: Vec<RoleMembership>,
        grants: Vec<ExecuteGrant>,
        local_peer_credentials: Vec<LocalPeerCredential>,
    ) -> Result<Self, SecuritySnapshotError> {
        Self::new_with_function_targets_and_local_peer_credentials(
            revision,
            functions
                .into_iter()
                .map(SecurityFunctionTarget::application)
                .collect(),
            principals,
            memberships,
            grants,
            local_peer_credentials,
        )
    }

    /// Creates a security snapshot from the closed two-class target union.
    pub fn new_with_function_targets(
        revision: RevisionPair,
        functions: Vec<SecurityFunctionTarget>,
        principals: Vec<Principal>,
        memberships: Vec<RoleMembership>,
        grants: Vec<ExecuteGrant>,
    ) -> Result<Self, SecuritySnapshotError> {
        Self::new_with_function_targets_and_local_peer_credentials(
            revision,
            functions,
            principals,
            memberships,
            grants,
            vec![],
        )
    }

    /// Creates a two-class security snapshot with protected local peer credentials.
    ///
    /// The function set is the canonical, identity-ordered union of the pinned
    /// application catalogue and the exact verified standard snapshot. A
    /// function identity repeated across the two classes, or twice in either
    /// class, is a duplicate and fails construction closed.
    pub fn new_with_function_targets_and_local_peer_credentials(
        revision: RevisionPair,
        functions: Vec<SecurityFunctionTarget>,
        principals: Vec<Principal>,
        memberships: Vec<RoleMembership>,
        grants: Vec<ExecuteGrant>,
        local_peer_credentials: Vec<LocalPeerCredential>,
    ) -> Result<Self, SecuritySnapshotError> {
        Self::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            revision,
            functions,
            principals,
            memberships,
            grants,
            local_peer_credentials,
            vec![],
        )
    }

    /// Creates a two-class security snapshot with protected local peer
    /// credentials and privilege-class grants.
    ///
    /// The function set is the canonical, identity-ordered union of the
    /// pinned application catalogue and the exact verified standard
    /// snapshot. A function identity repeated across the two classes, or
    /// twice in either class, is a duplicate and fails construction closed.
    /// Privilege grants must be mutually distinct and must name a principal
    /// in the snapshot; a grant object is not required to be a member of
    /// the known function union because privilege grants may name sealed
    /// system functions.
    pub fn new_with_function_targets_local_peer_credentials_and_privilege_grants(
        revision: RevisionPair,
        functions: Vec<SecurityFunctionTarget>,
        principals: Vec<Principal>,
        memberships: Vec<RoleMembership>,
        grants: Vec<ExecuteGrant>,
        local_peer_credentials: Vec<LocalPeerCredential>,
        privilege_grants: Vec<PrivilegeGrant>,
    ) -> Result<Self, SecuritySnapshotError> {
        let mut known_functions = BTreeMap::new();
        for function in functions {
            if known_functions
                .insert(function.function, function)
                .is_some()
            {
                return Err(SecuritySnapshotError::DuplicateFunction);
            }
        }

        let mut principals_by_id = BTreeMap::new();
        for principal in principals {
            if principal.id == PrincipalId::from_bytes([0; 16]) {
                return Err(SecuritySnapshotError::EmptyPrincipal);
            }
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
            if !known_functions.contains_key(&grant.function) {
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

        let mut validated_privilege_grants = BTreeSet::new();
        for privilege_grant in privilege_grants {
            if !validated_privilege_grants.insert(privilege_grant) {
                return Err(SecuritySnapshotError::DuplicatePrivilegeGrant);
            }
            if !principals_by_id.contains_key(&privilege_grant.grantee) {
                return Err(SecuritySnapshotError::UnknownPrivilegeGrantPrincipal);
            }
            if let Some(object) = privilege_grant.object()
                && !known_functions.contains_key(&object)
                && system_function_by_id(object).is_none()
            {
                return Err(SecuritySnapshotError::UnknownPrivilegeGrantObject);
            }
        }

        let security_context_digest = security_snapshot_digest(
            revision,
            &known_functions,
            &principals_by_id,
            &validated_memberships,
            &validated_grants,
            &validated_privilege_grants,
            &local_peers_by_uid,
        );

        Ok(Self {
            revision,
            function_targets: known_functions,
            principals: principals_by_id,
            memberships: validated_memberships,
            grants: validated_grants,
            privilege_grants: validated_privilege_grants,
            local_peer_credentials: local_peers_by_uid,
            security_context_digest,
        })
    }

    /// Returns the active revision pair that this snapshot authorises.
    pub const fn revision(&self) -> RevisionPair {
        self.revision
    }

    /// Returns the immutable canonical digest of this validated security snapshot.
    ///
    /// The digest is local decision evidence only; it is not a transport or
    /// audit field.
    pub const fn security_context_digest(&self) -> Sha256Digest {
        self.security_context_digest
    }

    /// Iterates over known functions in canonical identity order.
    pub fn functions(&self) -> impl Iterator<Item = FunctionId> + '_ {
        self.function_targets.keys().copied()
    }

    /// Iterates over the closed two-class function targets in canonical identity order.
    pub fn function_targets(&self) -> impl Iterator<Item = SecurityFunctionTarget> + '_ {
        self.function_targets.values().copied()
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

    /// Iterates over privilege-class grants ordered by grantee, class, and
    /// object.
    pub fn privilege_grants(&self) -> impl Iterator<Item = PrivilegeGrant> + '_ {
        self.privilege_grants.iter().copied()
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
            binding: AuthenticatedSessionBinding::new(),
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
                if self
                    .principals
                    .get(&membership.role)
                    .is_some_and(|role| role.status == PrincipalStatus::Active)
                    && reached.insert(membership.role)
                {
                    pending.push(membership.role);
                }
            }
        }

        reached
    }

    /// Decides whether the authenticated session may execute the pinned target.
    ///
    /// The target must name a function in the canonical two-class union with a
    /// matching closed class. A class-less target is an `Application` target,
    /// so the raw dispatcher stays closed to every verified-standard function.
    /// A verified-standard target must carry the exact immutable standard
    /// snapshot revision and executable function revision recorded by this
    /// snapshot; a current, different, or unverified standard snapshot is
    /// denied before any grant is considered.
    ///
    /// Both legacy function grants and durable `PrivilegeGrant::Execute`
    /// grants may authorise the target. Durable grants apply when class-wide
    /// or object-scoped to the target function. Selection is deterministic:
    /// the direct session principal is preferred, then the lowest active role;
    /// either grant model is sufficient for the selected principal, and the
    /// selected principal is retained as the authorising evidence.
    pub fn authorise_execute(
        &self,
        session: &AuthenticatedSession,
        target: InvocationTarget,
    ) -> ExecuteDecision {
        if let Err(reason) = self.validate_session_and_revision(session, target) {
            return ExecuteDecision::Denied(reason);
        }
        let Some(function_target) = self.function_targets.get(&target.function) else {
            return ExecuteDecision::Denied(ExecuteDenial::UnknownFunction);
        };
        let target_class = target.class().unwrap_or(TargetClass::Application);
        if function_target.class != target_class {
            return ExecuteDecision::Denied(ExecuteDenial::UnknownFunction);
        }
        if target_class == TargetClass::VerifiedStandard
            && (function_target.executable_revision != target.executable_revision()
                || function_target.standard_revision != target.standard_revision())
        {
            return ExecuteDecision::Denied(ExecuteDenial::UnknownFunction);
        }
        let has_execute_grant = |grantee| {
            self.grants
                .contains(&ExecuteGrant::new(grantee, target.function))
                || self.privilege_grants.iter().any(|grant| {
                    grant.grantee() == grantee
                        && grant.class() == PrivilegeClass::Execute
                        && (grant.is_class_wide() || grant.object() == Some(target.function))
                })
        };
        let authorising_principal = if has_execute_grant(session.principal) {
            session.principal
        } else if let Some(role) = session
            .active_roles
            .iter()
            .copied()
            .filter(|role| has_execute_grant(*role))
            .min()
        {
            role
        } else {
            return ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant);
        };

        ExecuteDecision::Allowed(self.allowed_invocation(session, target, authorising_principal))
    }

    /// Decides whether an authenticated session may enter a sealed system function.
    ///
    /// This closed rule admits only an exact identity in the mandatory system
    /// registry. It does not grant any application function or authorise a
    /// target carried by a system-function request.
    pub fn authorise_system_function(
        &self,
        session: &AuthenticatedSession,
        target: InvocationTarget,
    ) -> ExecuteDecision {
        if let Err(reason) = self.validate_session_and_revision(session, target) {
            return ExecuteDecision::Denied(reason);
        }
        self.authorise_system_function_after_validation(session, target)
    }

    /// Checks the exact sealed entry required by one protected invocation.
    ///
    /// This crate-private operation is not a target authorisation. It admits
    /// only the `sys.invoke` registry identity before request processing.
    pub(crate) fn authorise_sys_invoke_entry(
        &self,
        session: &AuthenticatedSession,
        target: InvocationTarget,
    ) -> ExecuteDecision {
        if target.function != SYS_INVOKE_FUNCTION_ID {
            return ExecuteDecision::Denied(ExecuteDenial::UnknownFunction);
        }
        self.authorise_system_function(session, target)
    }

    fn authorise_system_function_after_validation(
        &self,
        session: &AuthenticatedSession,
        target: InvocationTarget,
    ) -> ExecuteDecision {
        if target.class.is_some()
            || target.standard_revision.is_some()
            || target.executable_revision.is_some()
        {
            return ExecuteDecision::Denied(ExecuteDenial::UnknownFunction);
        }
        if system_function_by_id(target.function).is_none() {
            return ExecuteDecision::Denied(ExecuteDenial::UnknownFunction);
        }
        ExecuteDecision::Allowed(self.allowed_invocation(session, target, session.principal))
    }

    /// Decides whether an authenticated session may execute catalogue health.
    ///
    /// This closed system rule applies only to the exact reserved health
    /// identity. It does not grant any application function.
    pub fn authorise_catalogue_health(
        &self,
        session: &AuthenticatedSession,
        target: InvocationTarget,
    ) -> ExecuteDecision {
        if let Err(reason) = self.validate_session_and_revision(session, target) {
            return ExecuteDecision::Denied(reason);
        }
        if target.function != CATALOGUE_HEALTH_FUNCTION_ID {
            return ExecuteDecision::Denied(ExecuteDenial::UnknownFunction);
        }
        self.authorise_system_function_after_validation(session, target)
    }

    fn validate_session_and_revision(
        &self,
        session: &AuthenticatedSession,
        target: InvocationTarget,
    ) -> Result<(), ExecuteDenial> {
        self.bind_authenticated_session(session.principal, session.active_roles.clone())
            .map_err(|_| ExecuteDenial::InvalidSession)?;
        if target.revision != self.revision {
            return Err(ExecuteDenial::RevisionMismatch);
        }
        Ok(())
    }

    fn allowed_invocation(
        &self,
        session: &AuthenticatedSession,
        target: InvocationTarget,
        authorising_principal: PrincipalId,
    ) -> AuthorisedInvocation {
        AuthorisedInvocation {
            session_principal: session.principal,
            effective_principal: session.principal,
            active_roles: session.active_roles.clone(),
            authorising_principal,
            target,
            security_context_digest: self.security_context_digest,
        }
    }
}

#[cfg(test)]
mod tests;
