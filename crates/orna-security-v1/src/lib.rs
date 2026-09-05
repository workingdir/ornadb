//! A bounded session-security state machine.
//!
//! This crate deliberately performs no TLS, socket, clock, entropy-provider, or
//! session-store I/O. Integrations supply those capabilities through the small
//! adapter traits below; no request payload may select a provider or authority.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use subtle::ConstantTimeEq;

pub const CREDENTIAL_BYTES: usize = 32;

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

#[derive(Clone, Eq, PartialEq)]
pub struct OpaqueCredential([u8; CREDENTIAL_BYTES]);

impl OpaqueCredential {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; CREDENTIAL_BYTES]) -> Self {
        Self(bytes)
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
        let outcome = match current.take() {
            None => AttachOutcome::Attached,
            Some(Attachment::Active(existing)) => AttachOutcome::Replaced(existing),
            Some(Attachment::Disconnected(lease)) if now <= lease.expires_at => {
                AttachOutcome::Reconnected
            }
            Some(Attachment::Disconnected(_)) => return Err(BoundaryError::Expired),
        };
        *current = Some(Attachment::Active(attachment));
        Ok(outcome)
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

    fn expire(&mut self, session: SessionId, now: u64) -> Result<(), BoundaryError> {
        let record = self
            .sessions
            .get_mut(&session)
            .ok_or(BoundaryError::Denied)?;
        if now >= record.expires_at {
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
            boundary.attach(session, &app, &credential, attachment(3), 8),
            Ok(AttachOutcome::Reconnected)
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

    #[test]
    fn diagnostics_never_render_secret_material() {
        let credential = OpaqueCredential::from_bytes([0x5a; CREDENTIAL_BYTES]);
        let rendered = format!("{credential:?} {}", BoundaryError::Denied);
        assert!(rendered.contains("REDACTED"));
        assert!(!rendered.contains("5a"));
        assert!(!rendered.contains("app.example"));
    }
}
