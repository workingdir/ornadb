//! A bounded session-security state machine.
//!
//! This crate deliberately performs no TLS, socket, clock, entropy-provider, or
//! session-store I/O. Integrations supply those capabilities through the small
//! adapter traits below; secret storage is the explicit SOPS-backed exception.
//! No request payload may select a provider or authority.

use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use subtle::ConstantTimeEq;

mod sops;

pub use sops::SopsProvider;

pub const CREDENTIAL_BYTES: usize = 32;
/// Maximum session lease permitted by the live protocol profile.
///
/// Hosts may advertise shorter leases, but an admitted session must never
/// retain owner state beyond this bounded interval.
pub const MAX_SESSION_LEASE: u64 = 300_000;

/// A stable, non-secret name for one externally managed secret.
///
/// The reference is descriptive data, not a resolution capability. The SOPS
/// provider maps names to opaque hashed filenames; references never select a
/// path, decryption identity, or external authority.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SecretRef(String);

impl SecretRef {
    /// Creates a reference from its stable name.
    ///
    /// Names are intentionally not assigned a provider-specific grammar. Empty
    /// and control-containing names are rejected so a canonical textual
    /// reference cannot contain framing or diagnostic separators.
    ///
    /// # Errors
    ///
    /// Returns [`SecretBoundaryError::InvalidReference`] for an empty or
    /// control-containing name.
    pub fn new(value: impl Into<String>) -> Result<Self, SecretBoundaryError> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(SecretBoundaryError::InvalidReference);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for SecretRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Non-sensitive metadata exposed for a secret reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SecretMetadata {
    reference: SecretRef,
    provider: String,
    available: bool,
}

impl SecretMetadata {
    #[must_use]
    pub fn new(reference: SecretRef, provider: impl Into<String>, available: bool) -> Self {
        Self {
            reference,
            provider: provider.into(),
            available,
        }
    }

    #[must_use]
    pub fn reference(&self) -> &SecretRef {
        &self.reference
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub const fn available(&self) -> bool {
        self.available
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretBoundaryError {
    InvalidReference,
    Denied,
    Unavailable,
}

impl fmt::Display for SecretBoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidReference => "invalid secret reference",
            Self::Denied => "secret resolution denied",
            Self::Unavailable => "secret unavailable",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SecretBoundaryError {}

/// Adapter-owned secret boundary.
///
/// `metadata` returns only stable, non-secret facts. Providers may probe
/// availability by decrypting into memory and immediately discarding the
/// result. `with_secret` is the explicit privileged resolution operation;
/// implementations must decrypt in memory and return
/// [`SecretBoundaryError::Unavailable`] when the external identity is absent.
/// The callback is the only byte-bearing surface exposed by this crate, so
/// resolved material has no public `Debug`, `Display`, or serialization API.
pub trait SecretResolver {
    /// Returns non-sensitive metadata without disclosing resolved secret bytes.
    ///
    /// # Errors
    ///
    /// Returns [`SecretBoundaryError::Denied`] when metadata access is denied.
    /// Providers report ordinary unavailability with
    /// [`SecretMetadata::available`] set to `false`.
    fn metadata(&self, reference: &SecretRef) -> Result<SecretMetadata, SecretBoundaryError>;

    /// Resolves the secret for one explicitly privileged callback.
    ///
    /// # Errors
    ///
    /// Returns a provider-owned boundary error when disclosure is denied or
    /// the external decryption identity is unavailable.
    fn with_secret<T>(
        &self,
        reference: &SecretRef,
        operation: &mut dyn FnMut(&[u8]) -> T,
    ) -> Result<T, SecretBoundaryError>;
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId([u8; 16]);

impl SessionId {
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttachmentId([u8; 16]);

impl AttachmentId {
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Origin(String);

impl Origin {
    /// Parses one canonical, scheme-and-authority origin.
    ///
    /// # Errors
    ///
    /// Returns `Denied` when the input is not a bounded origin.
    pub fn parse(value: impl Into<String>) -> Result<Self, BoundaryError> {
        let value = value.into();
        let Some((scheme, authority)) = value.split_once("://") else {
            return Err(BoundaryError::Denied);
        };
        if scheme.is_empty()
            || authority.is_empty()
            || authority.contains(['/', '?', '#', '@'])
            || authority.chars().any(char::is_whitespace)
            || scheme.chars().any(|character| {
                !character.is_ascii_alphanumeric()
                    && character != '+'
                    && character != '-'
                    && character != '.'
            })
        {
            return Err(BoundaryError::Denied);
        }
        let canonical = format!(
            "{}://{}",
            scheme.to_ascii_lowercase(),
            authority.to_ascii_lowercase()
        );
        Ok(Self(canonical))
    }
}

#[derive(Clone, Debug, Default)]
pub struct OriginPolicy {
    allow: BTreeSet<Origin>,
    deny: BTreeSet<Origin>,
}

/// Explicit host/operator policy controlling whether source may be disclosed for remote analysis.
/// The default is local-only; transport configuration never implies consent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RemoteAnalysisPolicy {
    /// No content, dependency, resource, or query data leaves the host.
    #[default]
    LocalOnly,
    /// Remote disclosure is permitted only for analyses whose declared cost does not exceed
    /// `max_cost_units`. The ceiling is host/operator authority: no per-analysis acknowledgement
    /// can raise it.
    AllowRemoteDisclosure { max_cost_units: u64 },
}

/// Caller-supplied consent to nontrivial cost for one analysis. It narrows what the policy
/// permits and never raises the policy ceiling.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CostAcknowledgement {
    #[default]
    Withheld,
    Given,
}

/// Exact provenance and disclosure boundary for one remote analysis attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteAnalysisRequest {
    decision: RemoteAnalysisDecision,
    source_snapshot: String,
    disclosed_data: Vec<String>,
    declared_cost_units: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteAnalysisDecision {
    DeniedLocalOnly,
    DeniedCostExceedsPolicy,
    DeniedCostUnacknowledged,
    AllowedByUserPolicy,
}

impl RemoteAnalysisRequest {
    /// Makes an auditable request record. It does not perform network I/O.
    /// The snapshot identity must identify the exact admitted source/dependency/resource inputs.
    /// Any nonzero `declared_cost_units` is nontrivial: it needs the policy ceiling to cover it
    /// and `CostAcknowledgement::Given`. A denied request discloses nothing, so its disclosed
    /// boundary is empty.
    #[must_use]
    pub fn new(
        policy: RemoteAnalysisPolicy,
        source_snapshot: impl Into<String>,
        disclosed_data: impl IntoIterator<Item = String>,
        declared_cost_units: u64,
        acknowledgement: CostAcknowledgement,
    ) -> Self {
        let decision = match policy {
            RemoteAnalysisPolicy::LocalOnly => RemoteAnalysisDecision::DeniedLocalOnly,
            RemoteAnalysisPolicy::AllowRemoteDisclosure { max_cost_units }
                if declared_cost_units > max_cost_units =>
            {
                RemoteAnalysisDecision::DeniedCostExceedsPolicy
            }
            RemoteAnalysisPolicy::AllowRemoteDisclosure { .. }
                if declared_cost_units > 0 && acknowledgement == CostAcknowledgement::Withheld =>
            {
                RemoteAnalysisDecision::DeniedCostUnacknowledged
            }
            RemoteAnalysisPolicy::AllowRemoteDisclosure { .. } => {
                RemoteAnalysisDecision::AllowedByUserPolicy
            }
        };
        let disclosed_data = if decision == RemoteAnalysisDecision::AllowedByUserPolicy {
            disclosed_data.into_iter().collect()
        } else {
            Vec::new()
        };
        Self {
            decision,
            source_snapshot: source_snapshot.into(),
            disclosed_data,
            declared_cost_units,
        }
    }

    #[must_use]
    pub fn decision(&self) -> RemoteAnalysisDecision {
        self.decision
    }

    #[must_use]
    pub fn source_snapshot(&self) -> &str {
        &self.source_snapshot
    }

    #[must_use]
    pub fn disclosed_data(&self) -> &[String] {
        &self.disclosed_data
    }

    #[must_use]
    pub fn declared_cost_units(&self) -> u64 {
        self.declared_cost_units
    }

    #[must_use]
    pub fn is_allowed(&self) -> bool {
        self.decision == RemoteAnalysisDecision::AllowedByUserPolicy
    }
}

/// Identity of a retained artifact a reproduction depends on: a retained remote output, or the
/// retained model artifacts (one identity, e.g. a manifest digest, covering the set). It is
/// constructed only through `new`, which refuses an empty or control-character identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedArtifact(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedArtifactError {
    Empty,
    ControlCharacter,
}

impl RetainedArtifact {
    /// # Errors
    ///
    /// Returns `Empty` for an empty identity and `ControlCharacter` when it contains a control
    /// character.
    pub fn new(identity: impl Into<String>) -> Result<Self, RetainedArtifactError> {
        let identity = identity.into();
        if identity.is_empty() {
            return Err(RetainedArtifactError::Empty);
        }
        if identity.chars().any(char::is_control) {
            return Err(RetainedArtifactError::ControlCharacter);
        }
        Ok(Self(identity))
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.0
    }
}

/// How a recorded remote result can be reproduced. Source snapshot provenance is not a
/// reproducibility claim: a changing external service may not reproduce the same result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reproducibility {
    /// The retained output is replayed; the external service is not called again.
    RetainedOutputReplay(RetainedArtifact),
    /// The analysis is re-run, and only with the retained model artifacts identified here.
    ReExecutionWithRetainedArtifacts(RetainedArtifact),
    NotReproducible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteAnalysisRecordError {
    /// The policy did not allow the request, so nothing may be recorded as disclosed.
    NotAllowed(RemoteAnalysisDecision),
}

/// Audit record of one allowed remote analysis: provenance, disclosed boundary, declared cost,
/// and a reproducibility claim that must be stated explicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedRemoteAnalysis {
    source_snapshot: String,
    disclosed_data: Vec<String>,
    declared_cost_units: u64,
    reproducibility: Reproducibility,
}

impl RecordedRemoteAnalysis {
    /// # Errors
    ///
    /// Returns `NotAllowed` with the request's decision when `request` is not allowed.
    pub fn record(
        request: &RemoteAnalysisRequest,
        reproducibility: Reproducibility,
    ) -> Result<Self, RemoteAnalysisRecordError> {
        if !request.is_allowed() {
            return Err(RemoteAnalysisRecordError::NotAllowed(request.decision()));
        }
        Ok(Self {
            source_snapshot: request.source_snapshot.clone(),
            disclosed_data: request.disclosed_data.clone(),
            declared_cost_units: request.declared_cost_units,
            reproducibility,
        })
    }

    #[must_use]
    pub fn source_snapshot(&self) -> &str {
        &self.source_snapshot
    }

    #[must_use]
    pub fn disclosed_data(&self) -> &[String] {
        &self.disclosed_data
    }

    #[must_use]
    pub fn declared_cost_units(&self) -> u64 {
        self.declared_cost_units
    }

    #[must_use]
    pub fn reproducibility(&self) -> &Reproducibility {
        &self.reproducibility
    }
}

impl OriginPolicy {
    #[must_use]
    pub fn new(
        allow: impl IntoIterator<Item = Origin>,
        deny: impl IntoIterator<Item = Origin>,
    ) -> Self {
        Self {
            allow: allow.into_iter().collect(),
            deny: deny.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn permits(&self, origin: &Origin) -> bool {
        !self.deny.contains(origin) && self.allow.contains(origin)
    }
}

#[cfg(test)]
mod remote_analysis_tests {
    use super::*;

    const SNAPSHOT: &str = "git:source=abc;deps=def;resources=ghi;toolchain=jkl;abi=mno";

    fn disclosed() -> Vec<String> {
        ["source", "dependency-snapshot", "query"]
            .map(str::to_owned)
            .to_vec()
    }

    fn allowed_within(max_cost_units: u64) -> RemoteAnalysisPolicy {
        RemoteAnalysisPolicy::AllowRemoteDisclosure { max_cost_units }
    }

    #[test]
    fn local_only_policy_refuses_disclosure_even_when_cost_is_acknowledged() {
        let request = RemoteAnalysisRequest::new(
            RemoteAnalysisPolicy::default(),
            SNAPSHOT,
            disclosed(),
            0,
            CostAcknowledgement::Given,
        );
        assert_eq!(request.decision(), RemoteAnalysisDecision::DeniedLocalOnly);
        assert!(!request.is_allowed());
        assert!(request.disclosed_data().is_empty());
        assert_eq!(request.source_snapshot(), SNAPSHOT);
        assert_eq!(
            RecordedRemoteAnalysis::record(&request, Reproducibility::NotReproducible),
            Err(RemoteAnalysisRecordError::NotAllowed(
                RemoteAnalysisDecision::DeniedLocalOnly
            ))
        );
    }

    #[test]
    fn zero_cost_disclosure_needs_no_cost_acknowledgement_and_records_the_boundary() {
        let request = RemoteAnalysisRequest::new(
            allowed_within(10),
            SNAPSHOT,
            disclosed(),
            0,
            CostAcknowledgement::Withheld,
        );
        assert_eq!(
            request.decision(),
            RemoteAnalysisDecision::AllowedByUserPolicy
        );
        assert_eq!(request.disclosed_data(), disclosed().as_slice());
    }

    #[test]
    fn nontrivial_cost_within_ceiling_requires_per_analysis_acknowledgement() {
        let withheld = RemoteAnalysisRequest::new(
            allowed_within(10),
            SNAPSHOT,
            disclosed(),
            5,
            CostAcknowledgement::Withheld,
        );
        assert_eq!(
            withheld.decision(),
            RemoteAnalysisDecision::DeniedCostUnacknowledged
        );
        assert!(withheld.disclosed_data().is_empty());

        let given = RemoteAnalysisRequest::new(
            allowed_within(10),
            SNAPSHOT,
            disclosed(),
            5,
            CostAcknowledgement::Given,
        );
        assert!(given.is_allowed());
        assert_eq!(given.declared_cost_units(), 5);
    }

    #[test]
    fn cost_above_host_ceiling_is_refused_even_when_acknowledged() {
        let over = RemoteAnalysisRequest::new(
            allowed_within(10),
            SNAPSHOT,
            disclosed(),
            11,
            CostAcknowledgement::Given,
        );
        assert_eq!(
            over.decision(),
            RemoteAnalysisDecision::DeniedCostExceedsPolicy
        );
        assert!(over.disclosed_data().is_empty());

        let at_ceiling = RemoteAnalysisRequest::new(
            allowed_within(10),
            SNAPSHOT,
            disclosed(),
            10,
            CostAcknowledgement::Given,
        );
        assert!(at_ceiling.is_allowed());
    }

    #[test]
    fn recording_refuses_every_request_the_policy_did_not_allow() {
        let denied = [
            RemoteAnalysisRequest::new(
                RemoteAnalysisPolicy::LocalOnly,
                SNAPSHOT,
                disclosed(),
                0,
                CostAcknowledgement::Given,
            ),
            RemoteAnalysisRequest::new(
                allowed_within(10),
                SNAPSHOT,
                disclosed(),
                5,
                CostAcknowledgement::Withheld,
            ),
            RemoteAnalysisRequest::new(
                allowed_within(10),
                SNAPSHOT,
                disclosed(),
                11,
                CostAcknowledgement::Given,
            ),
        ];
        for request in &denied {
            assert_eq!(
                RecordedRemoteAnalysis::record(request, Reproducibility::NotReproducible),
                Err(RemoteAnalysisRecordError::NotAllowed(request.decision()))
            );
        }
    }

    #[test]
    fn provenance_snapshot_is_not_a_reproducibility_claim() {
        let request = RemoteAnalysisRequest::new(
            allowed_within(0),
            SNAPSHOT,
            disclosed(),
            0,
            CostAcknowledgement::Withheld,
        );
        let recorded =
            RecordedRemoteAnalysis::record(&request, Reproducibility::NotReproducible).unwrap();
        assert_eq!(recorded.source_snapshot(), SNAPSHOT);
        assert_eq!(recorded.disclosed_data(), disclosed().as_slice());
        assert_eq!(recorded.declared_cost_units(), 0);
        assert_eq!(
            recorded.reproducibility(),
            &Reproducibility::NotReproducible
        );
    }

    #[test]
    fn replay_and_reexecution_are_distinct_claims_over_a_retained_artifact() {
        let artifact = RetainedArtifact::new("model-artifacts:sha256:abc").unwrap();
        let request = RemoteAnalysisRequest::new(
            allowed_within(0),
            SNAPSHOT,
            disclosed(),
            0,
            CostAcknowledgement::Withheld,
        );
        let replay = RecordedRemoteAnalysis::record(
            &request,
            Reproducibility::RetainedOutputReplay(artifact.clone()),
        )
        .unwrap();
        let reexecution = RecordedRemoteAnalysis::record(
            &request,
            Reproducibility::ReExecutionWithRetainedArtifacts(artifact.clone()),
        )
        .unwrap();
        assert_eq!(
            replay.reproducibility(),
            &Reproducibility::RetainedOutputReplay(artifact.clone())
        );
        assert_eq!(
            reexecution.reproducibility(),
            &Reproducibility::ReExecutionWithRetainedArtifacts(artifact)
        );
        assert_ne!(replay.reproducibility(), reexecution.reproducibility());
    }

    #[test]
    fn retained_artifact_refuses_empty_and_control_character_identities() {
        assert_eq!(RetainedArtifact::new(""), Err(RetainedArtifactError::Empty));
        assert_eq!(
            RetainedArtifact::new("model\nartifact"),
            Err(RetainedArtifactError::ControlCharacter)
        );
        assert_eq!(
            RetainedArtifact::new("model:sha256:abc")
                .unwrap()
                .identity(),
            "model:sha256:abc"
        );
    }
}

/// A TLS implementation supplies only the verified origin; this crate never opens TLS.
pub trait TlsPeerAdapter {
    /// Returns the origin authenticated by the TLS implementation.
    ///
    /// # Errors
    ///
    /// Returns a redacted boundary failure when TLS authentication failed.
    fn verified_origin(&self) -> Result<Origin, BoundaryError>;
}

/// A socket implementation supplies a stable attachment identity; this crate never reads sockets.
pub trait SocketAdapter {
    fn attachment_id(&self) -> AttachmentId;
}

/// Entropy is supplied by a trusted provider adapter. Production implementations must be CSPRNG-backed.
pub trait CredentialIssuer {
    /// Issues exactly one opaque credential.
    ///
    /// # Errors
    ///
    /// Returns a redacted boundary failure when trusted entropy is unavailable.
    fn issue_credential(&mut self) -> Result<[u8; CREDENTIAL_BYTES], BoundaryError>;
}

/// Deletion remains adapter-owned. An adapter failure closes the in-memory session first.
pub trait SessionDeletionAdapter {
    type Error;

    /// Deletes or invalidates durable session state.
    ///
    /// # Errors
    ///
    /// Returns the adapter-specific deletion failure. The boundary closes first.
    fn delete(&mut self, session: SessionId) -> Result<(), Self::Error>;
}

#[derive(Clone, Eq)]
pub struct OpaqueCredential([u8; CREDENTIAL_BYTES]);

impl OpaqueCredential {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; CREDENTIAL_BYTES]) -> Self {
        Self(bytes)
    }
}

impl PartialEq for OpaqueCredential {
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for OpaqueCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueCredential([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Lease {
    expires_at: u64,
}

impl Lease {
    #[must_use]
    pub const fn expires_at(self) -> u64 {
        self.expires_at
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachOutcome {
    Attached,
    Replaced(AttachmentId),
    Reconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundaryError {
    Denied,
    Closed,
    Expired,
    DeletionFailed,
}

impl fmt::Display for BoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Denied => "session request denied",
            Self::Closed => "session closed",
            Self::Expired => "session lease expired",
            Self::DeletionFailed => "session closed after deletion failure",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for BoundaryError {}

#[derive(Clone)]
enum Attachment {
    Active(AttachmentId),
    Disconnected(Lease),
}

#[derive(Clone)]
enum State {
    Open {
        credential: OpaqueCredential,
        attachment: Option<Attachment>,
    },
    Closed,
}

#[derive(Clone)]
struct Session {
    origin: Origin,
    expires_at: u64,
    state: State,
}

pub struct SessionBoundary {
    policy: OriginPolicy,
    reconnect_for: u64,
    sessions: HashMap<SessionId, Session>,
}

impl SessionBoundary {
    #[must_use]
    pub fn new(policy: OriginPolicy, reconnect_for: u64) -> Self {
        Self {
            policy,
            reconnect_for,
            sessions: HashMap::new(),
        }
    }

    /// Creates an un-attached session after origin and lifetime admission.
    ///
    /// # Errors
    ///
    /// Returns `Denied` for an unapproved origin, duplicate session, expired
    /// lifetime, or issuer failure.
    pub fn create(
        &mut self,
        session: SessionId,
        origin: Origin,
        expires_at: u64,
        now: u64,
        issuer: &mut impl CredentialIssuer,
    ) -> Result<OpaqueCredential, BoundaryError> {
        if !self.policy.permits(&origin)
            || expires_at <= now
            || expires_at.saturating_sub(now) > MAX_SESSION_LEASE
            || self.sessions.contains_key(&session)
        {
            return Err(BoundaryError::Denied);
        }
        let credential = OpaqueCredential::from_bytes(issuer.issue_credential()?);
        self.sessions.insert(
            session,
            Session {
                origin,
                expires_at,
                state: State::Open {
                    credential: credential.clone(),
                    attachment: None,
                },
            },
        );
        Ok(credential)
    }

    /// Replaces the current credential while preserving the bounded session.
    ///
    /// # Errors
    ///
    /// Returns a closed, expired, or denied result when current credentials do
    /// not authorise rotation, or when issuance fails.
    pub fn rotate(
        &mut self,
        session: SessionId,
        origin: &Origin,
        credential: &OpaqueCredential,
        now: u64,
        issuer: &mut impl CredentialIssuer,
    ) -> Result<OpaqueCredential, BoundaryError> {
        self.authorise(session, origin, credential, now)?;
        let replacement = OpaqueCredential::from_bytes(issuer.issue_credential()?);
        let record = self
            .sessions
            .get_mut(&session)
            .ok_or(BoundaryError::Denied)?;
        if let State::Open { credential, .. } = &mut record.state {
            *credential = replacement.clone();
            Ok(replacement)
        } else {
            Err(BoundaryError::Closed)
        }
    }

    /// Authenticates and attaches one socket, replacing any active attachment.
    ///
    /// # Errors
    ///
    /// Returns a closed, expired, or denied result for invalid session state,
    /// origin, credential, or reconnect lease.
    pub fn attach(
        &mut self,
        session: SessionId,
        origin: &Origin,
        credential: &OpaqueCredential,
        attachment: AttachmentId,
        now: u64,
    ) -> Result<AttachOutcome, BoundaryError> {
        self.authorise(session, origin, credential, now)?;
        let record = self
            .sessions
            .get_mut(&session)
            .ok_or(BoundaryError::Denied)?;
        let State::Open {
            attachment: current,
            ..
        } = &mut record.state
        else {
            return Err(BoundaryError::Closed);
        };
        let outcome = match current.as_ref() {
            None => AttachOutcome::Attached,
            Some(Attachment::Active(existing)) => AttachOutcome::Replaced(*existing),
            Some(Attachment::Disconnected(lease)) if now < lease.expires_at => {
                AttachOutcome::Reconnected
            }
            Some(Attachment::Disconnected(_)) => return Err(BoundaryError::Expired),
        };
        *current = Some(Attachment::Active(attachment));
        Ok(outcome)
    }

    /// Validates an attachment without changing the current attachment.
    /// Hosts use this before delivering a protocol handshake whose later
    /// commit will call [`Self::attach`].
    ///
    /// # Errors
    ///
    /// Returns the same admission error as [`Self::attach`] except that an
    /// expired session is not closed until the committing call.
    pub fn validate_attach(
        &self,
        session: SessionId,
        origin: &Origin,
        credential: &OpaqueCredential,
        now: u64,
    ) -> Result<(), BoundaryError> {
        let record = self.sessions.get(&session).ok_or(BoundaryError::Denied)?;
        let unattached = matches!(
            &record.state,
            State::Open {
                attachment: None,
                ..
            }
        );
        if unattached && now >= record.expires_at {
            return Err(BoundaryError::Expired);
        }
        match &record.state {
            State::Closed => Err(BoundaryError::Closed),
            State::Open {
                credential: expected,
                ..
            } if &record.origin == origin && constant_time_eq(&expected.0, &credential.0) => Ok(()),
            State::Open { .. } => Err(BoundaryError::Denied),
        }
    }

    /// Validates that an already-attached transport is still the session's
    /// active attachment at `now`.
    ///
    /// This is credential-free because a frame has already crossed the
    /// authenticated attachment boundary, but it must not continue operating
    /// after expiry, replacement, or revocation.
    ///
    /// # Errors
    ///
    /// Returns [`BoundaryError::Expired`] and closes the session when its
    /// lease has elapsed, or a stable closed/denied error when the attachment
    /// is no longer active for the supplied session.
    pub fn validate_active_attachment(
        &mut self,
        session: SessionId,
        attachment: AttachmentId,
        now: u64,
    ) -> Result<(), BoundaryError> {
        self.expire(session, now)?;
        let record = self.sessions.get(&session).ok_or(BoundaryError::Denied)?;
        match &record.state {
            State::Open {
                attachment: Some(Attachment::Active(current)),
                ..
            } if *current == attachment => Ok(()),
            State::Open { .. } => Err(BoundaryError::Denied),
            State::Closed => Err(BoundaryError::Closed),
        }
    }

    /// Releases the active attachment and starts its reconnect lease.
    ///
    /// # Errors
    ///
    /// Returns a closed, expired, or denied result unless the attachment is the
    /// session's sole active attachment.
    pub fn disconnect(
        &mut self,
        session: SessionId,
        attachment: AttachmentId,
        now: u64,
    ) -> Result<Lease, BoundaryError> {
        self.expire(session, now)?;
        let record = self
            .sessions
            .get_mut(&session)
            .ok_or(BoundaryError::Denied)?;
        let State::Open {
            attachment: current,
            ..
        } = &mut record.state
        else {
            return Err(BoundaryError::Closed);
        };
        if !matches!(current, Some(Attachment::Active(active)) if *active == attachment) {
            return Err(BoundaryError::Denied);
        }
        let lease = Lease {
            expires_at: now.saturating_add(self.reconnect_for),
        };
        *current = Some(Attachment::Disconnected(lease));
        Ok(lease)
    }

    /// Immediately closes a session and invalidates its credential.
    ///
    /// # Errors
    ///
    /// Returns `Denied` if the session does not exist.
    pub fn revoke(&mut self, session: SessionId) -> Result<(), BoundaryError> {
        let record = self
            .sessions
            .get_mut(&session)
            .ok_or(BoundaryError::Denied)?;
        record.state = State::Closed;
        Ok(())
    }

    /// Closes first, then asks the storage adapter to delete durable state.
    ///
    /// # Errors
    ///
    /// Returns `DeletionFailed` if the adapter fails; the session remains
    /// closed in either case.
    pub fn delete(
        &mut self,
        session: SessionId,
        adapter: &mut impl SessionDeletionAdapter,
    ) -> Result<(), BoundaryError> {
        let record = self
            .sessions
            .get_mut(&session)
            .ok_or(BoundaryError::Denied)?;
        record.state = State::Closed;
        adapter
            .delete(session)
            .map_err(|_| BoundaryError::DeletionFailed)
    }

    fn authorise(
        &mut self,
        session: SessionId,
        origin: &Origin,
        credential: &OpaqueCredential,
        now: u64,
    ) -> Result<(), BoundaryError> {
        self.expire(session, now)?;
        let record = self.sessions.get(&session).ok_or(BoundaryError::Denied)?;
        match &record.state {
            State::Closed => Err(BoundaryError::Closed),
            State::Open {
                credential: expected,
                ..
            } if &record.origin == origin && constant_time_eq(&expected.0, &credential.0) => Ok(()),
            State::Open { .. } => Err(BoundaryError::Denied),
        }
    }

    /// Applies the creation deadline only while no socket has ever attached.
    /// Per `30-protocol.md` the reconnection lease starts when the WebSocket is
    /// lost, so an attached or disconnected session is governed by its lease.
    fn expire(&mut self, session: SessionId, now: u64) -> Result<(), BoundaryError> {
        let record = self
            .sessions
            .get_mut(&session)
            .ok_or(BoundaryError::Denied)?;
        let attached = matches!(
            &record.state,
            State::Open {
                attachment: Some(_),
                ..
            }
        );
        if !attached && now >= record.expires_at {
            record.state = State::Closed;
            return Err(BoundaryError::Expired);
        }
        Ok(())
    }
}

fn constant_time_eq(left: &[u8; CREDENTIAL_BYTES], right: &[u8; CREDENTIAL_BYTES]) -> bool {
    left.ct_eq(right).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Issuer(u8);
    impl CredentialIssuer for Issuer {
        fn issue_credential(&mut self) -> Result<[u8; CREDENTIAL_BYTES], BoundaryError> {
            let value = self.0;
            self.0 = self.0.wrapping_add(1);
            Ok([value; CREDENTIAL_BYTES])
        }
    }

    struct DeleteFails;
    impl SessionDeletionAdapter for DeleteFails {
        type Error = ();
        fn delete(&mut self, _: SessionId) -> Result<(), Self::Error> {
            Err(())
        }
    }

    struct Resolver;

    impl SecretResolver for Resolver {
        fn metadata(&self, reference: &SecretRef) -> Result<SecretMetadata, SecretBoundaryError> {
            Ok(SecretMetadata::new(reference.clone(), "sops", true))
        }

        fn with_secret<T>(
            &self,
            _: &SecretRef,
            operation: &mut dyn FnMut(&[u8]) -> T,
        ) -> Result<T, SecretBoundaryError> {
            Ok(operation(b"not-for-diagnostics"))
        }
    }

    fn origin(value: &str) -> Origin {
        Origin::parse(value).unwrap()
    }
    fn id(value: u8) -> SessionId {
        SessionId::new([value; 16])
    }
    fn attachment(value: u8) -> AttachmentId {
        AttachmentId::new([value; 16])
    }
    fn boundary() -> SessionBoundary {
        SessionBoundary::new(
            OriginPolicy::new(
                [origin("https://app.example")],
                [origin("https://blocked.example")],
            ),
            5,
        )
    }

    #[test]
    fn origin_policy_is_exact_allow_and_deny_wins() {
        let policy = OriginPolicy::new(
            [
                origin("https://app.example"),
                origin("https://blocked.example"),
            ],
            [origin("https://blocked.example")],
        );
        assert!(policy.permits(&origin("HTTPS://APP.EXAMPLE")));
        assert!(!policy.permits(&origin("https://blocked.example")));
        assert!(!policy.permits(&origin("https://foreign.example")));
        assert!(Origin::parse("https://app.example/path").is_err());
    }

    #[test]
    fn rotation_rejects_replayed_and_foreign_credentials() {
        let mut boundary = boundary();
        let mut issuer = Issuer(1);
        let session = id(1);
        let app = origin("https://app.example");
        let old = boundary
            .create(session, app.clone(), 20, 0, &mut issuer)
            .unwrap();
        let replacement = boundary
            .rotate(session, &app, &old, 1, &mut issuer)
            .unwrap();
        assert_ne!(old, replacement);
        assert_eq!(
            boundary.attach(session, &app, &old, attachment(1), 2),
            Err(BoundaryError::Denied)
        );
        assert_eq!(
            boundary.attach(
                session,
                &origin("https://foreign.example"),
                &replacement,
                attachment(1),
                2
            ),
            Err(BoundaryError::Denied)
        );
        assert_eq!(
            boundary.attach(id(2), &app, &replacement, attachment(1), 2),
            Err(BoundaryError::Denied)
        );
    }

    #[test]
    fn replacement_attachment_and_reconnect_lease_are_bounded() {
        let mut boundary = boundary();
        let mut issuer = Issuer(1);
        let session = id(1);
        let app = origin("https://app.example");
        let credential = boundary
            .create(session, app.clone(), 20, 0, &mut issuer)
            .unwrap();
        assert_eq!(
            boundary.attach(session, &app, &credential, attachment(1), 1),
            Ok(AttachOutcome::Attached)
        );
        assert_eq!(
            boundary.attach(session, &app, &credential, attachment(2), 2),
            Ok(AttachOutcome::Replaced(attachment(1)))
        );
        assert_eq!(
            boundary
                .disconnect(session, attachment(2), 3)
                .unwrap()
                .expires_at(),
            8
        );
        assert_eq!(
            boundary.attach(session, &app, &credential, attachment(3), 7),
            Ok(AttachOutcome::Reconnected)
        );
    }

    #[test]
    fn session_lease_is_bounded_at_the_protocol_maximum() {
        let mut boundary = boundary();
        let mut issuer = Issuer(1);
        let app = origin("https://app.example");

        assert!(
            boundary
                .create(id(1), app.clone(), MAX_SESSION_LEASE, 0, &mut issuer)
                .is_ok()
        );
        assert_eq!(
            boundary.create(
                id(2),
                app,
                MAX_SESSION_LEASE.saturating_add(1),
                0,
                &mut issuer,
            ),
            Err(BoundaryError::Denied)
        );
    }

    #[test]
    fn expired_reconnect_attempt_cannot_consume_the_disconnected_lease() {
        let mut boundary = boundary();
        let mut issuer = Issuer(1);
        let session = id(1);
        let app = origin("https://app.example");
        let credential = boundary
            .create(session, app.clone(), 20, 0, &mut issuer)
            .unwrap();
        boundary
            .attach(session, &app, &credential, attachment(1), 1)
            .unwrap();
        boundary.disconnect(session, attachment(1), 2).unwrap();

        assert_eq!(
            boundary.attach(session, &app, &credential, attachment(2), 7),
            Err(BoundaryError::Expired)
        );
        assert_eq!(
            boundary.attach(session, &app, &credential, attachment(2), 7),
            Err(BoundaryError::Expired)
        );
    }

    #[test]
    fn expiry_revocation_and_deletion_failure_close_fail_closed() {
        let mut boundary = boundary();
        let mut issuer = Issuer(1);
        let session = id(1);
        let app = origin("https://app.example");
        let credential = boundary
            .create(session, app.clone(), 3, 0, &mut issuer)
            .unwrap();
        assert_eq!(
            boundary.attach(session, &app, &credential, attachment(1), 3),
            Err(BoundaryError::Expired)
        );
        assert_eq!(
            boundary.attach(session, &app, &credential, attachment(1), 2),
            Err(BoundaryError::Closed)
        );
        let session = id(2);
        let credential = boundary
            .create(session, app.clone(), 10, 0, &mut issuer)
            .unwrap();
        assert_eq!(
            boundary.delete(session, &mut DeleteFails),
            Err(BoundaryError::DeletionFailed)
        );
        assert_eq!(
            boundary.attach(session, &app, &credential, attachment(2), 1),
            Err(BoundaryError::Closed)
        );
        let revoked = id(3);
        let revoked_credential = boundary
            .create(revoked, app.clone(), 10, 0, &mut issuer)
            .unwrap();
        boundary.revoke(revoked).unwrap();
        assert_eq!(
            boundary.attach(revoked, &app, &revoked_credential, attachment(3), 1),
            Err(BoundaryError::Closed)
        );
    }

    /// `30-protocol.md`: "Loss of the WebSocket starts the advertised finite
    /// reconnection lease." An attached session is therefore not closed at its
    /// creation deadline; only a detached session expires, `reconnect_for`
    /// milliseconds after detachment.
    #[test]
    fn attached_session_is_governed_by_the_reconnect_lease() {
        let mut boundary = boundary();
        let mut issuer = Issuer(1);
        let session = id(7);
        let app = origin("https://app.example");
        let credential = boundary
            .create(session, app.clone(), 3, 0, &mut issuer)
            .unwrap();
        let active = attachment(7);
        boundary
            .attach(session, &app, &credential, active, 1)
            .unwrap();

        assert_eq!(
            boundary.validate_active_attachment(session, active, 2),
            Ok(())
        );
        assert_eq!(
            boundary.validate_active_attachment(session, active, 100),
            Ok(())
        );

        boundary.disconnect(session, active, 100).unwrap();
        let reconnect = attachment(8);
        assert!(matches!(
            boundary.attach(session, &app, &credential, reconnect, 104),
            Ok(AttachOutcome::Reconnected)
        ));
        assert_eq!(
            boundary.validate_active_attachment(session, reconnect, 104),
            Ok(())
        );

        boundary.disconnect(session, reconnect, 104).unwrap();
        assert_eq!(
            boundary.attach(session, &app, &credential, attachment(9), 109),
            Err(BoundaryError::Expired)
        );
    }

    #[test]
    fn diagnostics_never_render_secret_material() {
        let credential = OpaqueCredential::from_bytes([0x5a; CREDENTIAL_BYTES]);
        let rendered = format!("{credential:?} {}", BoundaryError::Denied);
        assert!(rendered.contains("REDACTED"));
        assert!(!rendered.contains("5a"));
        assert!(!rendered.contains("app.example"));
    }

    #[test]
    fn secret_reference_and_metadata_never_contain_secret_bytes() {
        let reference = SecretRef::new("messages.inbox").unwrap();
        assert_eq!(reference.as_str(), "messages.inbox");
        assert_eq!(reference.to_string(), "messages.inbox");
        assert!(SecretRef::new("").is_err());
        assert!(SecretRef::new("messages\ninbox").is_err());

        let resolver = Resolver;
        let metadata = resolver.metadata(&reference).unwrap();
        let debug = format!("{metadata:?}");
        assert!(metadata.available());
        assert_eq!(metadata.provider(), "sops");
        assert_eq!(metadata.reference(), &reference);
        assert!(!debug.contains("not-for-diagnostics"));

        let mut callback = |secret: &[u8]| secret.len();
        assert_eq!(resolver.with_secret(&reference, &mut callback), Ok(19));
    }
}
