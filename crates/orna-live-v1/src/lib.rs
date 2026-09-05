//! A small transport-facing adapter for canonical `orna.present.v1` sessions.
//!
//! Hosts provide request routing, credential issuance, and durable deletion;
//! this crate owns no socket or HTTP implementation. All public failures are
//! stable redacted codes and inbound protocol bytes are decoded canonically.

use std::collections::BTreeMap;

use orna_protocol_v1::{Envelope, Limits as ProtocolLimits, Message};
use orna_security_v1::{
    AttachOutcome, AttachmentId, BoundaryError, CredentialIssuer, OpaqueCredential, Origin,
    SessionBoundary, SessionDeletionAdapter, SessionId,
};
use orna_serving_v1::{
    Credential as ServingCredential, Error as ServingError, Origin as ServingOrigin, Serving,
};

pub const SUBPROTOCOL: &str = "orna.present.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_text_bytes: usize,
    pub protocol: ProtocolLimits,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_text_bytes: 4 * 1024,
            protocol: ProtocolLimits::default(),
        }
    }
}

impl Limits {
    fn validate(self) -> Result<Self> {
        if self.max_text_bytes == 0 {
            return Err(Error::Limit);
        }
        self.protocol.validate().map_err(|_| Error::Limit)?;
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Limit,
    UnsupportedSubprotocol,
    InvalidFrame,
    InvalidMessage,
    Denied,
    Closed,
    ReplayRequired,
    DeletionFailed,
}

impl Error {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Limit => "live.limit",
            Self::UnsupportedSubprotocol => "live.unsupported_subprotocol",
            Self::InvalidFrame => "live.invalid_frame",
            Self::InvalidMessage => "live.invalid_message",
            Self::Denied => "live.denied",
            Self::Closed => "live.closed",
            Self::ReplayRequired => "live.replay_required",
            Self::DeletionFailed => "live.deletion_failed",
        }
    }
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Frame {
    Binary(Vec<u8>),
    Text(String),
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameOutcome {
    Accepted,
    Cancelled,
    Resync { revisions: usize },
    Closed,
}

/// Host-issued identity for a session. The two credential representations are
/// intentionally separate opaque values for their respective existing APIs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionCredential {
    pub security: OpaqueCredential,
    pub serving: ServingCredential,
}

pub struct CreateRequest<'a> {
    pub id: [u8; 16],
    pub origin: Origin,
    pub expires_at: u64,
    pub now: u64,
    pub subscribe: &'a [u8],
}
pub struct ResumeRequest<'a> {
    pub id: [u8; 16],
    pub origin: &'a Origin,
    pub credential: &'a SessionCredential,
    pub attachment: [u8; 16],
    pub now: u64,
}
pub struct DeleteRequest {
    pub id: [u8; 16],
}

/// HTTP boundary contract. A router maps `POST /v1/live/sessions` to create,
/// `POST /v1/live/sessions/{id}/attachments` to resume, and
/// `DELETE /v1/live/sessions/{id}` to delete. Origins and credentials have
/// already been parsed into their opaque types before reaching this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HttpBody {
    Session(SessionCredential),
    Empty,
    ErrorCode(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(&'static str, &'static str)>,
    pub body: HttpBody,
}

impl HttpResponse {
    fn error(error: Error) -> Self {
        Self {
            status: match error {
                Error::Denied => 403,
                Error::Closed => 410,
                Error::Limit => 413,
                Error::UnsupportedSubprotocol => 426,
                Error::DeletionFailed
                | Error::InvalidFrame
                | Error::InvalidMessage
                | Error::ReplayRequired => 400,
            },
            headers: Vec::new(),
            body: HttpBody::ErrorCode(error.code()),
        }
    }
}

/// The only side effect the adapter asks a host to perform while deleting.
pub trait DeletionAdapter: SessionDeletionAdapter {}
impl<T: SessionDeletionAdapter> DeletionAdapter for T {}

/// Concrete async request/frame host adapter. Its async methods make it fit
/// ordinary async routers while keeping I/O explicit at the edges.
pub struct LiveHost {
    limits: Limits,
    security: SessionBoundary,
    serving: Serving,
    attachments: BTreeMap<[u8; 16], [u8; 16]>,
}

// The state boundary is synchronous today, but the async methods keep the
// adapter signature ready for hosts whose credential and deletion adapters
// perform I/O. The explicit lint allowance records that deliberate seam.
#[allow(clippy::unused_async)]
impl LiveHost {
    /// Creates a bounded live host around the supplied security and serving
    /// state machines.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Limit`] when any configured protocol bound is zero.
    pub fn new(limits: Limits, security: SessionBoundary, serving: Serving) -> Result<Self> {
        Ok(Self {
            limits: limits.validate()?,
            security,
            serving,
            attachments: BTreeMap::new(),
        })
    }

    /// Selects the required live-protocol WebSocket subprotocol.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedSubprotocol`] when the client did not
    /// offer the required protocol.
    pub fn negotiate_subprotocol(offered: &[&str]) -> Result<&'static str> {
        offered
            .contains(&SUBPROTOCOL)
            .then_some(SUBPROTOCOL)
            .ok_or(Error::UnsupportedSubprotocol)
    }

    /// Admits a canonical subscribe envelope as a new authenticated session.
    ///
    /// # Errors
    ///
    /// Returns a redacted boundary error when the envelope, origin,
    /// credential issuer, or serving admission is rejected.
    pub async fn create(
        &mut self,
        request: CreateRequest<'_>,
        issuer: &mut impl LiveCredentialIssuer,
    ) -> Result<SessionCredential> {
        let subscribe = self.decode(request.subscribe)?;
        let security = self
            .security
            .create(
                session_id(request.id),
                request.origin,
                request.expires_at,
                request.now,
                issuer,
            )
            .map_err(map_boundary)?;
        // The serving credential is opaque too; the host's issuer supplies its
        // same trusted material to both APIs without placing it in diagnostics.
        let serving = issuer
            .last_issued()
            .map(ServingCredential::new)
            .ok_or(Error::Denied)?;
        if let Err(error) = self.serving.admit(
            request.id,
            serving.clone(),
            ServingOrigin(request.id),
            &subscribe,
        ) {
            let _ = self.security.revoke(session_id(request.id));
            return Err(map_serving(error));
        }
        Ok(SessionCredential { security, serving })
    }

    /// HTTP `POST /v1/live/sessions`: `201`, header
    /// `content-type: application/orna-live-v1`, and opaque session body.
    pub async fn http_create(
        &mut self,
        request: CreateRequest<'_>,
        issuer: &mut impl LiveCredentialIssuer,
    ) -> HttpResponse {
        match self.create(request, issuer).await {
            Ok(credential) => HttpResponse {
                status: 201,
                headers: vec![("content-type", "application/orna-live-v1")],
                body: HttpBody::Session(credential),
            },
            Err(error) => HttpResponse::error(error),
        }
    }

    /// Reattaches one authenticated WebSocket and replaces an older
    /// attachment when necessary.
    ///
    /// # Errors
    ///
    /// Returns a redacted boundary error when the session, credential,
    /// attachment, or serving state is not admissible.
    pub async fn resume(&mut self, request: ResumeRequest<'_>) -> Result<AttachOutcome> {
        let outcome = self
            .security
            .attach(
                session_id(request.id),
                request.origin,
                &request.credential.security,
                attachment_id(request.attachment),
                request.now,
            )
            .map_err(map_boundary)?;
        if let Err(error) = self
            .serving
            .reconnect(request.id, &request.credential.serving)
        {
            let _ = self.security.revoke(session_id(request.id));
            self.attachments.retain(|_, session| *session != request.id);
            return Err(map_serving(error));
        }
        if matches!(outcome, AttachOutcome::Replaced(_)) {
            self.attachments.retain(|_, session| *session != request.id);
        }
        self.attachments.insert(request.attachment, request.id);
        Ok(outcome)
    }

    /// HTTP WebSocket upgrade `POST /v1/live/sessions/{id}/attachments`:
    /// `101`, headers `upgrade: websocket` and `sec-websocket-protocol:
    /// orna.present.v1`, empty body. The actual HTTP server performs the
    /// upgrade after receiving this response.
    pub async fn http_resume(&mut self, request: ResumeRequest<'_>) -> HttpResponse {
        match self.resume(request).await {
            Ok(_) => HttpResponse {
                status: 101,
                headers: vec![
                    ("upgrade", "websocket"),
                    ("sec-websocket-protocol", SUBPROTOCOL),
                ],
                body: HttpBody::Empty,
            },
            Err(error) => HttpResponse::error(error),
        }
    }

    /// Closes the session before asking the host to delete its durable state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DeletionFailed`] when durable deletion fails; the
    /// in-memory session remains closed in that case.
    pub async fn delete(
        &mut self,
        request: DeleteRequest,
        deletion: &mut impl DeletionAdapter,
    ) -> Result<()> {
        let deleted = self
            .security
            .delete(session_id(request.id), deletion)
            .is_ok();
        self.attachments.retain(|_, session| *session != request.id);
        self.serving
            .credential_deleted(request.id, deleted)
            .map_err(|_| Error::DeletionFailed)?;
        Ok(())
    }

    /// HTTP `DELETE /v1/live/sessions/{id}`: `204` with an empty body. A
    /// deletion adapter failure returns a redacted `400` and has already
    /// closed both session state machines.
    pub async fn http_delete(
        &mut self,
        request: DeleteRequest,
        deletion: &mut impl DeletionAdapter,
    ) -> HttpResponse {
        match self.delete(request, deletion).await {
            Ok(()) => HttpResponse {
                status: 204,
                headers: vec![],
                body: HttpBody::Empty,
            },
            Err(error) => HttpResponse::error(error),
        }
    }

    /// Admits one complete WebSocket application frame.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol, serving, security, or size-boundary error;
    /// no partially decoded message is forwarded.
    pub async fn handle_frame(
        &mut self,
        attachment: [u8; 16],
        now: u64,
        frame: Frame,
    ) -> Result<FrameOutcome> {
        let session = *self.attachments.get(&attachment).ok_or(Error::Closed)?;
        match frame {
            Frame::Close => {
                self.attachments.remove(&attachment);
                self.security
                    .disconnect(session_id(session), attachment_id(attachment), now)
                    .map_err(map_boundary)?;
                self.serving.disconnect(session).map_err(map_serving)?;
                Ok(FrameOutcome::Closed)
            }
            Frame::Text(value) => {
                if value.len() > self.limits.max_text_bytes {
                    Err(Error::Limit)
                } else {
                    Err(Error::InvalidFrame)
                }
            }
            Frame::Binary(bytes) => {
                let envelope = self.decode(&bytes)?;
                match envelope.message {
                    Message::Cancel { .. } => {
                        self.serving
                            .cancel_envelope(session, &envelope)
                            .map_err(map_serving)?;
                        Ok(FrameOutcome::Cancelled)
                    }
                    Message::Resync => {
                        let revisions = self.serving.resync(session, 0).map_err(map_serving)?.len();
                        Ok(FrameOutcome::Resync { revisions })
                    }
                    _ => Err(Error::InvalidMessage),
                }
            }
        }
    }

    fn decode(&self, bytes: &[u8]) -> Result<Envelope> {
        if bytes.len() > self.limits.protocol.max_message_bytes {
            return Err(Error::Limit);
        }
        Envelope::decode(bytes, self.limits.protocol).map_err(|_| Error::InvalidMessage)
    }

    /// The execution host calls these around work it has accepted from a
    /// canonical request. Network cancellation then remains protocol-bound.
    /// Reserves one request identity for the attached session.
    ///
    /// # Errors
    ///
    /// Returns a redacted serving error when the session or request state is
    /// not admissible.
    pub fn reserve_request(&mut self, session: [u8; 16], request: [u8; 16]) -> Result<()> {
        self.serving
            .reserve_request(session, request)
            .map_err(map_serving)
    }

    /// Marks a previously reserved request as running.
    ///
    /// # Errors
    ///
    /// Returns a redacted serving error when the reservation is absent or
    /// terminal.
    pub fn start_request(&mut self, session: [u8; 16], request: [u8; 16]) -> Result<()> {
        self.serving
            .start_request(session, request)
            .map_err(map_serving)
    }
}

/// Issuers that also expose the last *just issued* credential let a host bind
/// the security and serving opaque wrappers without ever formatting bytes.
pub trait LiveCredentialIssuer: CredentialIssuer {
    fn last_issued(&self) -> Option<[u8; 32]>;
}

fn session_id(value: [u8; 16]) -> SessionId {
    SessionId::new(value)
}
fn attachment_id(value: [u8; 16]) -> AttachmentId {
    AttachmentId::new(value)
}
fn map_boundary(error: BoundaryError) -> Error {
    match error {
        BoundaryError::Closed | BoundaryError::Expired => Error::Closed,
        BoundaryError::DeletionFailed => Error::DeletionFailed,
        BoundaryError::Denied => Error::Denied,
    }
}
fn map_serving(error: ServingError) -> Error {
    match error {
        ServingError::ReplayRequired => Error::ReplayRequired,
        ServingError::SessionClosed => Error::Closed,
        _ => Error::Denied,
    }
}
