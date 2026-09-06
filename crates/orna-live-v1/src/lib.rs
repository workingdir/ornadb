//! A small transport-facing adapter for canonical `orna.present.v1` sessions.
//!
//! Hosts provide request routing, credential issuance, and durable deletion;
//! this crate owns bounded HTTP framing and an injectable byte-stream loop but
//! leaves bind, TLS, clock, and WebSocket ownership at the executable edge.
//! All public failures are stable redacted codes and inbound protocol bytes
//! are decoded canonically.

use futures::{
    executor::block_on,
    io::{AllowStdIo, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    io,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    pin::Pin,
};

use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_protocol_v1::{
    Envelope, Limits as ProtocolLimits, Message, RequestState, ResultBody, ResultStatus,
    TargetKind, canonical_request_fingerprint,
};
use orna_runtime_v1::{
    RequestIdentity, RequestState as DurableRequestState, RequestStatus as DurableRequestStatus,
    RuntimeError, RuntimeState, TerminalOutcome,
};
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
    UnsupportedOperation,
    RequestMismatch,
    ApplicationRejected,
    RuntimeUnavailable,
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
            Self::UnsupportedOperation => "live.unsupported_operation",
            Self::RequestMismatch => "wire.request_mismatch",
            Self::ApplicationRejected => "live.application_rejected",
            Self::RuntimeUnavailable => "live.runtime_unavailable",
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

/// A protocol outcome and, where supported by the retained state, its exact
/// canonical host response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchOutcome {
    pub outcome: FrameOutcome,
    pub response: Option<Envelope>,
}

/// Narrow seam for application-owned source execution. The adapter owns wire
/// admission and response identity; implementations must return a canonical
/// host response rather than an untyped success flag.
pub trait LiveApplication {
    /// # Errors
    ///
    /// Returns a stable redacted error when evaluation cannot admit the input.
    fn eval(&mut self, session: [u8; 16], request: [u8; 16], message: &Message)
    -> Result<Envelope>;
    /// # Errors
    ///
    /// Returns a stable redacted error when watching cannot admit the input.
    fn watch(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope>;

    /// Handles a client subscription after protocol admission.
    ///
    /// # Errors
    ///
    /// Returns a stable redacted error when the subscription cannot be served.
    fn subscribe(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope> {
        Err(Error::UnsupportedOperation)
    }

    /// Handles a client watch closure after protocol admission.
    ///
    /// # Errors
    ///
    /// Returns a stable redacted error when the watch cannot be closed.
    fn unsubscribe(
        &mut self,
        _: [u8; 16],
        _: [u8; 16],
        _: [u8; 32],
        _: &Message,
    ) -> Result<Envelope> {
        Err(Error::UnsupportedOperation)
    }

    /// Produces a fresh snapshot for an existing server watch.
    ///
    /// # Errors
    ///
    /// Returns a stable redacted error when a fresh snapshot cannot be served.
    fn resync(&mut self, _: [u8; 16], _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope> {
        Err(Error::UnsupportedOperation)
    }

    /// Handles one client event after protocol admission.
    ///
    /// # Errors
    ///
    /// Returns a stable redacted error when the event cannot be applied.
    fn event(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope> {
        Err(Error::UnsupportedOperation)
    }

    /// Handles a client cancellation after protocol admission.
    ///
    /// # Errors
    ///
    /// Returns a stable redacted error when the cancellation cannot be applied.
    fn cancel(&mut self, _: [u8; 16], _: [u8; 16], _: [u8; 32], _: &Message) -> Result<Envelope> {
        Err(Error::UnsupportedOperation)
    }
}

struct RejectApplication;
impl LiveApplication for RejectApplication {
    fn eval(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope> {
        Err(Error::UnsupportedOperation)
    }
    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope> {
        Err(Error::UnsupportedOperation)
    }
}

#[derive(Clone)]
struct RequestRecord {
    fingerprint: [u8; 32],
    terminal: Option<DispatchOutcome>,
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
                | Error::ReplayRequired
                | Error::UnsupportedOperation
                | Error::RequestMismatch
                | Error::ApplicationRejected => 400,
                Error::RuntimeUnavailable => 503,
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
    requests: BTreeMap<([u8; 16], [u8; 16]), RequestRecord>,
    watches: BTreeSet<([u8; 16], [u8; 16])>,
    runtime: Option<RuntimeState>,
}

enum DurableAdmission {
    Execute,
    Active,
    Replay(Box<DispatchOutcome>),
}

// The state boundary is synchronous today, but the async methods keep the
// adapter signature ready for hosts whose credential and deletion adapters
// perform I/O. The explicit lint allowance records that deliberate seam.
#[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
impl LiveHost {
    /// Creates a bounded live host around the supplied security and serving
    /// state machines.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Limit`] when any configured protocol bound is zero.
    pub fn new(limits: Limits, security: SessionBoundary, serving: Serving) -> Result<Self> {
        Self::build(limits, security, serving, None)
    }

    /// Creates a bounded live host whose request authority is the supplied
    /// durable runtime state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Limit`] when any configured protocol bound is zero.
    pub fn with_runtime_state(
        limits: Limits,
        security: SessionBoundary,
        serving: Serving,
        runtime: RuntimeState,
    ) -> Result<Self> {
        Self::build(limits, security, serving, Some(runtime))
    }

    fn build(
        limits: Limits,
        security: SessionBoundary,
        serving: Serving,
        runtime: Option<RuntimeState>,
    ) -> Result<Self> {
        Ok(Self {
            limits: limits.validate()?,
            security,
            serving,
            attachments: BTreeMap::new(),
            requests: BTreeMap::new(),
            watches: BTreeSet::new(),
            runtime,
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

    fn validate_resume(&self, request: &ResumeRequest<'_>) -> Result<()> {
        self.security
            .validate_attach(
                session_id(request.id),
                request.origin,
                &request.credential.security,
                request.now,
            )
            .map_err(map_boundary)?;
        self.serving
            .validate_reconnect(request.id, &request.credential.serving)
            .map_err(map_serving)
    }

    /// Rotates an authenticated session credential across both security and
    /// serving state without changing its attachment. HTTP resume uses this
    /// before the later cookie-authenticated WebSocket upgrade.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the credential, origin, session lease, or
    /// trusted credential issuer is not admissible.
    pub async fn rotate(
        &mut self,
        id: [u8; 16],
        origin: &Origin,
        credential: &SessionCredential,
        now: u64,
        issuer: &mut impl LiveCredentialIssuer,
    ) -> Result<SessionCredential> {
        let security = self
            .security
            .rotate(session_id(id), origin, &credential.security, now, issuer)
            .map_err(map_boundary)?;
        let serving = issuer
            .last_issued()
            .map(ServingCredential::new)
            .ok_or(Error::Denied)?;
        self.serving
            .rotate_credential(id, &credential.serving, serving.clone())
            .map_err(map_serving)?;
        Ok(SessionCredential { security, serving })
    }

    /// Rotates a session credential and retires its currently attached
    /// WebSocket, if any. The session and its serving state remain resumable;
    /// the executable host owns cancellation of the retired socket.
    ///
    /// # Errors
    ///
    /// Returns the same redacted credential, origin, lease, or serving error
    /// as [`Self::rotate`], or when retiring the active attachment fails.
    pub async fn rotate_and_retire(
        &mut self,
        id: [u8; 16],
        origin: &Origin,
        credential: &SessionCredential,
        now: u64,
        issuer: &mut impl LiveCredentialIssuer,
    ) -> Result<(SessionCredential, Option<[u8; 16]>)> {
        let retired = self
            .attachments
            .iter()
            .find_map(|(attachment, session)| (*session == id).then_some(*attachment));
        let replacement = self.rotate(id, origin, credential, now, issuer).await?;
        if let Some(attachment) = retired {
            self.security
                .disconnect(session_id(id), attachment_id(attachment), now)
                .map_err(map_boundary)?;
            self.serving.disconnect(id).map_err(map_serving)?;
            self.attachments.remove(&attachment);
        }
        Ok((replacement, retired))
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
        self.requests
            .retain(|(session, _), _| *session != request.id);
        self.watches.retain(|(session, _)| *session != request.id);
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
        let mut application = RejectApplication;
        Ok(self
            .dispatch_frame(attachment, now, frame, &mut application)
            .await?
            .outcome)
    }

    /// Idempotently closes one attachment owned by an executable socket
    /// worker. A worker may race retirement with its own EOF or frame error;
    /// an already-retired attachment is therefore reported as [`Error::Closed`]
    /// and has no effect on a replacement attachment.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Closed`] when the attachment has already been
    /// retired or otherwise left the host.
    pub async fn close_attachment(
        &mut self,
        attachment: [u8; 16],
        now: u64,
    ) -> Result<FrameOutcome> {
        let mut application = RejectApplication;
        Ok(self
            .dispatch_frame(attachment, now, Frame::Close, &mut application)
            .await?
            .outcome)
    }

    /// Dispatches one complete client frame through the full client registry.
    /// Application-owned operations are delegated through [`LiveApplication`];
    /// their typed canonical response is validated before it is retained.
    ///
    /// # Errors
    ///
    /// Returns stable bounded-frame, identity, or application errors.
    #[allow(clippy::too_many_lines)]
    pub async fn dispatch_frame(
        &mut self,
        attachment: [u8; 16],
        now: u64,
        frame: Frame,
        application: &mut impl LiveApplication,
    ) -> Result<DispatchOutcome> {
        let session = *self.attachments.get(&attachment).ok_or(Error::Closed)?;
        match frame {
            Frame::Close => {
                self.attachments.remove(&attachment);
                self.security
                    .disconnect(session_id(session), attachment_id(attachment), now)
                    .map_err(map_boundary)?;
                self.serving.disconnect(session).map_err(map_serving)?;
                Ok(DispatchOutcome {
                    outcome: FrameOutcome::Closed,
                    response: None,
                })
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
                let request = envelope.request.ok_or(Error::InvalidMessage)?;
                if matches!(
                    envelope.message,
                    Message::Snapshot { .. }
                        | Message::Delta { .. }
                        | Message::Result { .. }
                        | Message::Diagnostic { .. }
                        | Message::RequestStatusResult { .. }
                ) {
                    return Err(Error::InvalidMessage);
                }
                let fingerprint =
                    canonical_request_fingerprint(session, &envelope, self.limits.protocol)
                        .map_err(|_| Error::InvalidMessage)?;
                if let Message::Event {
                    fingerprint: sent, ..
                }
                | Message::Eval {
                    fingerprint: sent, ..
                } = &envelope.message
                    && *sent != fingerprint
                {
                    return Err(Error::RequestMismatch);
                }
                if self.runtime.is_some() {
                    match self
                        .admit_durable_request(session, request, fingerprint, &envelope)
                        .await?
                    {
                        DurableAdmission::Execute => {}
                        DurableAdmission::Active => {
                            self.requests.insert(
                                (session, request),
                                RequestRecord {
                                    fingerprint,
                                    terminal: None,
                                },
                            );
                            return Ok(DispatchOutcome {
                                outcome: FrameOutcome::Accepted,
                                response: None,
                            });
                        }
                        DurableAdmission::Replay(outcome) => {
                            self.requests.insert(
                                (session, request),
                                RequestRecord {
                                    fingerprint,
                                    terminal: Some((*outcome).clone()),
                                },
                            );
                            return Ok(*outcome);
                        }
                    }
                } else if let Some(record) = self.requests.get(&(session, request)) {
                    if record.fingerprint != fingerprint {
                        return Err(Error::RequestMismatch);
                    }
                    return Ok(record.terminal.clone().unwrap_or(DispatchOutcome {
                        outcome: FrameOutcome::Accepted,
                        response: None,
                    }));
                }
                if matches!(envelope.message, Message::Event { .. } | Message::Resync) {
                    let watch = envelope.watch.ok_or(Error::InvalidMessage)?;
                    if !self.watches.contains(&(session, watch)) {
                        return Err(Error::Denied);
                    }
                }
                if let Err(error) = self.reserve_and_start(session, request, fingerprint) {
                    self.retain_failure(session, request, fingerprint).await?;
                    return Err(error);
                }
                let dispatched: Result<DispatchOutcome> = async {
                    match &envelope.message {
                        Message::Subscribe { .. } => {
                            let response =
                                application.subscribe(session, request, &envelope.message)?;
                            let outcome = validate_snapshot_response(
                                request,
                                None,
                                response,
                                self.limits.protocol,
                            )?;
                            let watch = outcome
                                .response
                                .as_ref()
                                .and_then(|response| response.watch)
                                .ok_or(Error::ApplicationRejected)?;
                            self.open_watch(session, watch, &outcome)?;
                            self.complete(session, request, outcome).await
                        }
                        Message::Resync => {
                            let watch = envelope.watch.ok_or(Error::InvalidMessage)?;
                            let revisions =
                                self.serving.resync(session, 0).map_err(map_serving)?.len();
                            let response =
                                application.resync(session, request, watch, &envelope.message)?;
                            let outcome = validate_snapshot_response(
                                request,
                                Some(watch),
                                response,
                                self.limits.protocol,
                            )?;
                            self.complete(
                                session,
                                request,
                                DispatchOutcome {
                                    outcome: FrameOutcome::Resync { revisions },
                                    response: outcome.response,
                                },
                            )
                            .await
                        }
                        Message::Unsubscribe => {
                            let watch = envelope.watch.ok_or(Error::InvalidMessage)?;
                            let response = if self.watches.contains(&(session, watch)) {
                                application.unsubscribe(
                                    session,
                                    request,
                                    fingerprint,
                                    &envelope.message,
                                )?
                            } else {
                                unit_result_response(request, fingerprint)
                            };
                            let outcome = validate_control_response(
                                request,
                                fingerprint,
                                response,
                                self.limits.protocol,
                            )?;
                            self.serving
                                .close_watch(session, watch)
                                .map_err(map_serving)?;
                            self.watches.remove(&(session, watch));
                            self.complete(session, request, outcome).await
                        }
                        Message::Event { .. } => {
                            let response =
                                application.event(session, request, &envelope.message)?;
                            let outcome = validate_result_response(
                                request,
                                fingerprint,
                                response,
                                self.limits.protocol,
                            )?;
                            self.complete(session, request, outcome).await
                        }
                        Message::Eval { .. } => {
                            let response = application.eval(session, request, &envelope.message)?;
                            let outcome = validate_result_response(
                                request,
                                fingerprint,
                                response,
                                self.limits.protocol,
                            )?;
                            self.complete(session, request, outcome).await
                        }
                        Message::Watch { .. } => {
                            let response =
                                application.watch(session, request, &envelope.message)?;
                            let outcome = validate_snapshot_response(
                                request,
                                None,
                                response,
                                self.limits.protocol,
                            )?;
                            let watch = outcome
                                .response
                                .as_ref()
                                .and_then(|response| response.watch)
                                .ok_or(Error::ApplicationRejected)?;
                            self.open_watch(session, watch, &outcome)?;
                            self.complete(session, request, outcome).await
                        }
                        Message::Cancel {
                            target_kind,
                            target,
                        } => {
                            let target_active = match target_kind {
                                TargetKind::Request => {
                                    self.request_target_active(session, *target).await?
                                }
                                TargetKind::Watch => self.watches.contains(&(session, *target)),
                            };
                            let durable_request =
                                *target_kind == TargetKind::Request && self.runtime.is_some();
                            let target_cancelled = if target_active && durable_request {
                                self.cancel_target_request(session, *target).await?
                            } else {
                                false
                            };
                            let callback_allowed = target_active
                                && (*target_kind == TargetKind::Watch
                                    || if durable_request {
                                        target_cancelled
                                    } else {
                                        true
                                    });
                            let response = if callback_allowed {
                                let response = application.cancel(
                                    session,
                                    request,
                                    fingerprint,
                                    &envelope.message,
                                )?;
                                if *target_kind == TargetKind::Watch {
                                    self.serving
                                        .close_watch(session, *target)
                                        .map_err(map_serving)?;
                                    self.watches.remove(&(session, *target));
                                }
                                response
                            } else {
                                unit_result_response(request, fingerprint)
                            };
                            let outcome = validate_control_response(
                                request,
                                fingerprint,
                                response,
                                self.limits.protocol,
                            )?;
                            if target_active
                                && *target_kind == TargetKind::Request
                                && !durable_request
                            {
                                self.cancel_target_request(session, *target).await?;
                            }
                            self.complete(
                                session,
                                request,
                                DispatchOutcome {
                                    outcome: FrameOutcome::Cancelled,
                                    response: outcome.response,
                                },
                            )
                            .await
                        }
                        Message::RequestStatus {
                            target,
                            fingerprint: expected,
                        } => {
                            let retained = self.requests.get(&(session, *target));
                            if retained.is_some_and(|record| record.fingerprint != *expected) {
                                return Err(Error::RequestMismatch);
                            }
                            let (state, fingerprint, result) =
                                match self.serving.request_state(session, *target) {
                                    Ok(orna_serving_v1::RequestState::Reserved) => (
                                        RequestState::Reserved,
                                        retained.map(|record| record.fingerprint),
                                        None,
                                    ),
                                    Ok(orna_serving_v1::RequestState::Running) => (
                                        RequestState::Running,
                                        retained.map(|record| record.fingerprint),
                                        None,
                                    ),
                                    Ok(
                                        orna_serving_v1::RequestState::Cancelled
                                        | orna_serving_v1::RequestState::Completed,
                                    ) => (
                                        RequestState::Terminal,
                                        retained.map(|record| record.fingerprint),
                                        retained.and_then(|record| {
                                            retained_result_body(
                                                *target,
                                                *expected,
                                                record.terminal.as_ref(),
                                                self.limits.protocol,
                                            )
                                        }),
                                    ),
                                    Err(ServingError::RequestUnknown) => match &self.runtime {
                                        Some(runtime) => {
                                            let status = runtime
                                                .request_status(
                                                    RequestIdentity {
                                                        session_id: session,
                                                        request_id: *target,
                                                    },
                                                    *expected,
                                                )
                                                .await
                                                .map_err(|error| map_runtime(&error))?;
                                            match status {
                                                Some(status) => {
                                                    let state = match status.state {
                                                        DurableRequestState::Reserved => {
                                                            RequestState::Reserved
                                                        }
                                                        DurableRequestState::Running => {
                                                            RequestState::Running
                                                        }
                                                        DurableRequestState::Completed
                                                        | DurableRequestState::Cancelled => {
                                                            RequestState::Terminal
                                                        }
                                                        DurableRequestState::Orphaned => {
                                                            RequestState::Orphaned
                                                        }
                                                    };
                                                    let result = durable_result_body(
                                                        *target,
                                                        *expected,
                                                        &status,
                                                        self.limits.protocol,
                                                    );
                                                    (state, Some(status.fingerprint), result)
                                                }
                                                None => (RequestState::Unknown, None, None),
                                            }
                                        }
                                        None => (RequestState::Unknown, None, None),
                                    },
                                    Err(error) => return Err(map_serving(error)),
                                };
                            let outcome = DispatchOutcome {
                                outcome: FrameOutcome::Accepted,
                                response: Some(Envelope {
                                    request: Some(request),
                                    watch: None,
                                    message: Message::RequestStatusResult {
                                        target: *target,
                                        state,
                                        fingerprint,
                                        result,
                                    },
                                    extensions: BTreeMap::new(),
                                }),
                            };
                            self.complete(session, request, outcome).await
                        }
                        Message::Snapshot { .. }
                        | Message::Delta { .. }
                        | Message::Result { .. }
                        | Message::Diagnostic { .. }
                        | Message::RequestStatusResult { .. } => Err(Error::InvalidMessage),
                    }
                }
                .await;
                if dispatched.is_err() {
                    self.retain_failure(session, request, fingerprint).await?;
                }
                dispatched
            }
        }
    }

    async fn admit_durable_request(
        &self,
        session: [u8; 16],
        request: [u8; 16],
        fingerprint: [u8; 32],
        envelope: &Envelope,
    ) -> Result<DurableAdmission> {
        let runtime = self.runtime.as_ref().ok_or(Error::RuntimeUnavailable)?;
        let identity = RequestIdentity {
            session_id: session,
            request_id: request,
        };
        let (reserved, inserted) = runtime
            .reserve_request_with_admission(identity, fingerprint)
            .await
            .map_err(|error| map_runtime(&error))?;
        if !inserted {
            if reserved.state == DurableRequestState::Reserved {
                match runtime.start_request(identity, fingerprint).await {
                    Ok(_) => return Ok(DurableAdmission::Execute),
                    Err(RuntimeError::RequestStateConflict) => {
                        let current = runtime
                            .request_status_for_identity(identity)
                            .await
                            .map_err(|error| map_runtime(&error))?
                            .ok_or(Error::RuntimeUnavailable)?;
                        return self.durable_admission(current, envelope);
                    }
                    Err(error) => return Err(map_runtime(&error)),
                }
            }
            if reserved.state == DurableRequestState::Running {
                return self
                    .admit_running_request(
                        runtime,
                        identity,
                        session,
                        request,
                        fingerprint,
                        envelope,
                    )
                    .await;
            }
            return self.durable_admission(reserved, envelope);
        }
        match reserved.state {
            DurableRequestState::Reserved => {
                match runtime.start_request(identity, fingerprint).await {
                    Ok(_) => Ok(DurableAdmission::Execute),
                    Err(RuntimeError::RequestStateConflict) => {
                        let current = runtime
                            .request_status_for_identity(identity)
                            .await
                            .map_err(|error| map_runtime(&error))?
                            .ok_or(Error::RuntimeUnavailable)?;
                        self.durable_admission(current, envelope)
                    }
                    Err(error) => Err(map_runtime(&error)),
                }
            }
            DurableRequestState::Running => {
                self.admit_running_request(
                    runtime,
                    identity,
                    session,
                    request,
                    fingerprint,
                    envelope,
                )
                .await
            }
            DurableRequestState::Completed
            | DurableRequestState::Cancelled
            | DurableRequestState::Orphaned => self.durable_admission(reserved, envelope),
        }
    }

    async fn admit_running_request(
        &self,
        runtime: &RuntimeState,
        identity: RequestIdentity,
        session: [u8; 16],
        request: [u8; 16],
        fingerprint: [u8; 32],
        envelope: &Envelope,
    ) -> Result<DurableAdmission> {
        if self
            .requests
            .get(&(session, request))
            .is_some_and(|record| record.terminal.is_none())
        {
            return Ok(DurableAdmission::Active);
        }
        let outcome = retained_without_value_outcome(request, fingerprint);
        match runtime
            .orphan_request(identity, fingerprint, self.terminal_outcome(&outcome)?)
            .await
        {
            Ok(status) => self.durable_admission(status, envelope),
            Err(RuntimeError::RequestStateConflict) => {
                let current = runtime
                    .request_status_for_identity(identity)
                    .await
                    .map_err(|error| map_runtime(&error))?
                    .ok_or(Error::RuntimeUnavailable)?;
                self.durable_admission(current, envelope)
            }
            Err(error) => Err(map_runtime(&error)),
        }
    }

    async fn request_target_active(&self, session: [u8; 16], request: [u8; 16]) -> Result<bool> {
        match self.serving.request_state(session, request) {
            Ok(
                orna_serving_v1::RequestState::Reserved | orna_serving_v1::RequestState::Running,
            ) => Ok(true),
            Ok(
                orna_serving_v1::RequestState::Cancelled | orna_serving_v1::RequestState::Completed,
            ) => Ok(false),
            Err(ServingError::RequestUnknown) => {
                let Some(runtime) = &self.runtime else {
                    return Err(Error::Denied);
                };
                let status = runtime
                    .request_status_for_identity(RequestIdentity {
                        session_id: session,
                        request_id: request,
                    })
                    .await
                    .map_err(|error| map_runtime(&error))?
                    .ok_or(Error::Denied)?;
                Ok(!status.state.is_terminal())
            }
            Err(error) => Err(map_serving(error)),
        }
    }

    fn durable_admission(
        &self,
        status: DurableRequestStatus,
        envelope: &Envelope,
    ) -> Result<DurableAdmission> {
        if !status.state.is_terminal() {
            return Ok(DurableAdmission::Active);
        }
        let bytes = status
            .terminal_outcome
            .ok_or(Error::RuntimeUnavailable)?
            .into_bytes();
        let response = Envelope::decode(&bytes, self.limits.protocol)
            .map_err(|_| Error::RuntimeUnavailable)?;
        self.validate_retained_response(status.fingerprint, envelope, &response)?;
        let outcome = if matches!(envelope.message, Message::Cancel { .. })
            || matches!(
                response.message,
                Message::Result {
                    status: ResultStatus::Cancellation,
                    ..
                }
            ) {
            FrameOutcome::Cancelled
        } else if matches!(envelope.message, Message::Resync) {
            // The canonical response is durable, while the count of revisions
            // served during a resync belongs to the process-local watch state.
            FrameOutcome::Resync { revisions: 0 }
        } else {
            FrameOutcome::Accepted
        };
        Ok(DurableAdmission::Replay(Box::new(DispatchOutcome {
            outcome,
            response: Some(response),
        })))
    }

    fn validate_retained_response(
        &self,
        fingerprint: [u8; 32],
        envelope: &Envelope,
        response: &Envelope,
    ) -> Result<()> {
        let request = envelope.request.ok_or(Error::RuntimeUnavailable)?;
        let valid = match &envelope.message {
            Message::Subscribe { .. } | Message::Watch { .. } => {
                validate_snapshot_response(request, None, response.clone(), self.limits.protocol)
                    .is_ok()
            }
            Message::Resync => {
                let watch = envelope.watch.ok_or(Error::RuntimeUnavailable)?;
                validate_snapshot_response(
                    request,
                    Some(watch),
                    response.clone(),
                    self.limits.protocol,
                )
                .is_ok()
            }
            Message::Unsubscribe | Message::Cancel { .. } => validate_control_response(
                request,
                fingerprint,
                response.clone(),
                self.limits.protocol,
            )
            .is_ok(),
            Message::Event { .. } | Message::Eval { .. } => validate_result_response(
                request,
                fingerprint,
                response.clone(),
                self.limits.protocol,
            )
            .is_ok(),
            Message::RequestStatus { target, .. } => {
                validate_status_response(request, *target, response, self.limits.protocol).is_ok()
            }
            Message::Snapshot { .. }
            | Message::Delta { .. }
            | Message::Result { .. }
            | Message::Diagnostic { .. }
            | Message::RequestStatusResult { .. } => false,
        };
        valid.then_some(()).ok_or(Error::RuntimeUnavailable)
    }

    fn reserve_and_start(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        fingerprint: [u8; 32],
    ) -> Result<()> {
        self.requests.insert(
            (session, request),
            RequestRecord {
                fingerprint,
                terminal: None,
            },
        );
        let started = match self.serving.reserve_request(session, request) {
            Ok(()) => self
                .serving
                .start_request(session, request)
                .map_err(map_serving),
            Err(ServingError::RequestTerminal)
                if matches!(
                    self.serving.request_state(session, request),
                    Ok(orna_serving_v1::RequestState::Reserved)
                ) =>
            {
                self.serving
                    .start_request(session, request)
                    .map_err(map_serving)
            }
            Err(error) => Err(map_serving(error)),
        };
        if started.is_err() {
            self.requests.remove(&(session, request));
        }
        started
    }

    async fn complete(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        outcome: DispatchOutcome,
    ) -> Result<DispatchOutcome> {
        if let Some(runtime) = &self.runtime {
            let terminal = self.terminal_outcome(&outcome)?;
            runtime
                .complete_request(
                    RequestIdentity {
                        session_id: session,
                        request_id: request,
                    },
                    self.request_fingerprint(session, request)?,
                    terminal,
                )
                .await
                .map_err(|error| map_runtime(&error))?;
        }
        self.serving
            .complete_request(session, request)
            .map_err(map_serving)?;
        self.requests
            .get_mut(&(session, request))
            .ok_or(Error::Denied)?
            .terminal = Some(outcome.clone());
        Ok(outcome)
    }

    async fn retain_failure(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        fingerprint: [u8; 32],
    ) -> Result<()> {
        let failure = DispatchOutcome {
            outcome: FrameOutcome::Accepted,
            response: Some(Envelope {
                request: Some(request),
                watch: None,
                message: Message::Result {
                    status: ResultStatus::Failure,
                    value: None,
                    fingerprint,
                    diagnostic: None,
                },
                extensions: BTreeMap::new(),
            }),
        };
        if let Some(runtime) = &self.runtime {
            runtime
                .complete_request(
                    RequestIdentity {
                        session_id: session,
                        request_id: request,
                    },
                    fingerprint,
                    self.terminal_outcome(&failure)?,
                )
                .await
                .map_err(|error| map_runtime(&error))?;
        }
        if self.serving.complete_request(session, request).is_ok()
            && let Some(record) = self.requests.get_mut(&(session, request))
        {
            record.terminal = Some(failure);
            return Ok(());
        }
        let _ = self.serving.cancel_request(session, request);
        self.requests.remove(&(session, request));
        Ok(())
    }

    async fn cancel_target_request(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
    ) -> Result<bool> {
        let fingerprint = if let Some(record) = self.requests.get(&(session, request)) {
            record.fingerprint
        } else if let Some(runtime) = &self.runtime {
            let status = runtime
                .request_status_for_identity(RequestIdentity {
                    session_id: session,
                    request_id: request,
                })
                .await
                .map_err(|error| map_runtime(&error))?
                .ok_or(Error::Denied)?;
            if status.state.is_terminal() {
                return Ok(false);
            }
            status.fingerprint
        } else {
            self.serving
                .cancel_request(session, request)
                .map_err(map_serving)?;
            return Ok(true);
        };
        let outcome = DispatchOutcome {
            outcome: FrameOutcome::Cancelled,
            response: Some(Envelope {
                request: Some(request),
                watch: None,
                message: Message::Result {
                    status: ResultStatus::Cancellation,
                    value: None,
                    fingerprint,
                    diagnostic: None,
                },
                extensions: BTreeMap::new(),
            }),
        };
        if let Some(runtime) = &self.runtime {
            match runtime
                .cancel_request(
                    RequestIdentity {
                        session_id: session,
                        request_id: request,
                    },
                    fingerprint,
                    self.terminal_outcome(&outcome)?,
                )
                .await
            {
                Ok(_) => {}
                Err(RuntimeError::RequestStateConflict) => {
                    let status = runtime
                        .request_status_for_identity(RequestIdentity {
                            session_id: session,
                            request_id: request,
                        })
                        .await
                        .map_err(|error| map_runtime(&error))?
                        .ok_or(Error::RuntimeUnavailable)?;
                    if status.state.is_terminal() {
                        return Ok(false);
                    }
                    return Err(Error::RuntimeUnavailable);
                }
                Err(error) => return Err(map_runtime(&error)),
            }
        }
        match self.serving.cancel_request(session, request) {
            Ok(()) => {}
            Err(ServingError::RequestUnknown) if self.runtime.is_some() => {}
            Err(error) => return Err(map_serving(error)),
        }
        if let Some(record) = self.requests.get_mut(&(session, request)) {
            record.terminal = Some(outcome);
        }
        Ok(true)
    }

    fn request_fingerprint(&self, session: [u8; 16], request: [u8; 16]) -> Result<[u8; 32]> {
        self.requests
            .get(&(session, request))
            .map(|record| record.fingerprint)
            .ok_or(Error::RuntimeUnavailable)
    }

    fn terminal_outcome(&self, outcome: &DispatchOutcome) -> Result<TerminalOutcome> {
        let response = outcome.response.as_ref().ok_or(Error::RuntimeUnavailable)?;
        let bytes = response
            .encode(self.limits.protocol)
            .map_err(|_| Error::RuntimeUnavailable)?;
        TerminalOutcome::new(bytes).map_err(|error| map_runtime(&error))
    }

    fn open_watch(
        &mut self,
        session: [u8; 16],
        watch: [u8; 16],
        outcome: &DispatchOutcome,
    ) -> Result<()> {
        let Some(Envelope {
            message: Message::Snapshot { revision, .. },
            ..
        }) = outcome.response.as_ref()
        else {
            return Err(Error::ApplicationRejected);
        };
        self.serving
            .open_watch(session, watch, *revision)
            .map_err(map_serving)?;
        self.watches.insert((session, watch));
        Ok(())
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

fn retained_without_value_outcome(request: [u8; 16], fingerprint: [u8; 32]) -> DispatchOutcome {
    DispatchOutcome {
        outcome: FrameOutcome::Accepted,
        response: Some(Envelope {
            request: Some(request),
            watch: None,
            message: Message::Result {
                status: ResultStatus::RetainedWithoutValue,
                value: None,
                fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        }),
    }
}

fn retained_result_body(
    request: [u8; 16],
    fingerprint: [u8; 32],
    terminal: Option<&DispatchOutcome>,
    limits: ProtocolLimits,
) -> Option<ResultBody> {
    let response = terminal?.response.as_ref()?;
    validate_result_response(request, fingerprint, response.clone(), limits).ok()?;
    ResultBody::from_result(response, limits).ok()
}

fn durable_result_body(
    request: [u8; 16],
    fingerprint: [u8; 32],
    status: &DurableRequestStatus,
    limits: ProtocolLimits,
) -> Option<ResultBody> {
    let bytes = status.terminal_outcome.as_ref()?.as_bytes();
    let response = Envelope::decode(bytes, limits).ok()?;
    validate_result_response(request, fingerprint, response.clone(), limits).ok()?;
    ResultBody::from_result(&response, limits).ok()
}

fn validate_result_response(
    request: [u8; 16],
    fingerprint: [u8; 32],
    response: Envelope,
    limits: ProtocolLimits,
) -> Result<DispatchOutcome> {
    response
        .encode(limits)
        .map_err(|_| Error::ApplicationRejected)?;
    if response.request != Some(request) || response.watch.is_some() {
        return Err(Error::ApplicationRejected);
    }
    let Message::Result {
        fingerprint: returned,
        ..
    } = response.message
    else {
        return Err(Error::ApplicationRejected);
    };
    if returned != fingerprint {
        return Err(Error::RequestMismatch);
    }
    Ok(DispatchOutcome {
        outcome: FrameOutcome::Accepted,
        response: Some(response),
    })
}

fn validate_snapshot_response(
    request: [u8; 16],
    watch: Option<[u8; 16]>,
    response: Envelope,
    limits: ProtocolLimits,
) -> Result<DispatchOutcome> {
    response
        .encode(limits)
        .map_err(|_| Error::ApplicationRejected)?;
    if response.request != Some(request)
        || response.watch.is_none()
        || watch.is_some_and(|expected| response.watch != Some(expected))
    {
        return Err(Error::ApplicationRejected);
    }
    if !matches!(response.message, Message::Snapshot { .. }) {
        return Err(Error::ApplicationRejected);
    }
    Ok(DispatchOutcome {
        outcome: FrameOutcome::Accepted,
        response: Some(response),
    })
}

fn validate_control_response(
    request: [u8; 16],
    fingerprint: [u8; 32],
    response: Envelope,
    limits: ProtocolLimits,
) -> Result<DispatchOutcome> {
    let outcome = validate_result_response(request, fingerprint, response, limits)?;
    let Some(Envelope {
        message:
            Message::Result {
                status: ResultStatus::Success,
                value: Some(value),
                ..
            },
        ..
    }) = outcome.response.as_ref()
    else {
        return Err(Error::ApplicationRejected);
    };
    matches!(value.raw(), OvbRaw::Tag(60_014, unit) if matches!(unit.as_ref(), OvbRaw::Array(items) if items.is_empty()))
        .then_some(outcome)
        .ok_or(Error::ApplicationRejected)
}

fn validate_status_response(
    request: [u8; 16],
    target: [u8; 16],
    response: &Envelope,
    limits: ProtocolLimits,
) -> Result<()> {
    response
        .encode(limits)
        .map_err(|_| Error::ApplicationRejected)?;
    if response.request != Some(request) || response.watch.is_some() {
        return Err(Error::ApplicationRejected);
    }
    match &response.message {
        Message::RequestStatusResult {
            target: returned, ..
        } if *returned == target => Ok(()),
        _ => Err(Error::ApplicationRejected),
    }
}

fn unit_result_response(request: [u8; 16], fingerprint: [u8; 32]) -> Envelope {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Result {
            status: ResultStatus::Success,
            value: Some(CanonicalValue::unit()),
            fingerprint,
            diagnostic: None,
        },
        extensions: BTreeMap::new(),
    }
}

/// Issuers that also expose the last *just issued* credential let a host bind
/// the security and serving opaque wrappers without ever formatting bytes.
pub trait LiveCredentialIssuer: CredentialIssuer {
    fn last_issued(&self) -> Option<[u8; 32]>;
}

/// Production credential issuer backed by the operating system CSPRNG.
///
/// Entropy failures are reduced to the existing denied boundary error; the
/// transport never receives native provider details or a partially generated
/// credential.
#[derive(Debug, Default)]
pub struct SystemCredentialIssuer {
    last: Option<[u8; 32]>,
}

impl CredentialIssuer for SystemCredentialIssuer {
    fn issue_credential(&mut self) -> std::result::Result<[u8; 32], BoundaryError> {
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| BoundaryError::Denied)?;
        self.last = Some(token);
        Ok(token)
    }
}

impl LiveCredentialIssuer for SystemCredentialIssuer {
    fn last_issued(&self) -> Option<[u8; 32]> {
        self.last
    }
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

fn map_runtime(error: &RuntimeError) -> Error {
    match error {
        RuntimeError::RequestFingerprintMismatch => Error::RequestMismatch,
        _ => Error::RuntimeUnavailable,
    }
}

/// UUID and admission data supplied by the trusted database/runtime owner.
/// The transport never accepts filesystem paths or runtime identities from a
/// client request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionMetadata {
    pub session: [u8; 16],
    pub database: [u8; 16],
    pub runtime: [u8; 16],
    pub expires_at: u64,
    pub subscribe: Vec<u8>,
}

/// The application-specific admission seam required by the HTTP boundary.
/// Implementations allocate IDs and select exposed database/runtime state;
/// they do not expose credentials to callers.
pub trait LiveSessionAuthority {
    /// # Errors
    ///
    /// Returns a redacted error when the named exposed database cannot admit a
    /// new live session.
    fn create_session(&mut self, database: [u8; 16], now: u64) -> Result<SessionMetadata>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportLimits {
    pub max_request_bytes: usize,
    pub max_header_bytes: usize,
    pub max_frame_bytes: usize,
    pub lease_ms: u64,
    pub max_outgoing_bytes: usize,
    pub request_retention_ms: u64,
    pub tls: bool,
}

/// The operator-visible exposure selected when a live listener was bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListenerExposure {
    /// The default loopback-only listener.
    Loopback,
    /// An address deliberately selected by the operator.
    Explicit,
}

/// The address and exposure class that a bound live listener reports to its
/// executable host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListenerStatus {
    pub address: SocketAddr,
    pub exposure: ListenerExposure,
}

/// A live listener whose binding policy is explicit at the transport edge.
///
/// It owns only the bound socket. Accept loops, TLS, cancellation, clocks,
/// and application authority remain host-owned.
pub struct LiveListener {
    listener: TcpListener,
    status: ListenerStatus,
}

/// A runtime-owned way to accept a stream from a [`LiveListener`].
///
/// The listener retains the bound socket while the executable supplies the
/// asynchronous runtime integration. Implementations may box a runtime future
/// when it is not [`Unpin`].
pub trait LiveListenerAcceptor {
    type Accept<'a>: Future<Output = io::Result<std::net::TcpStream>> + Unpin
    where
        Self: 'a;

    /// Starts accepting one stream from the supplied owned listener.
    fn accept<'a>(&'a mut self, listener: &'a TcpListener) -> Self::Accept<'a>;
}

impl LiveListener {
    /// Returns the caller-owned accept handle.
    #[must_use]
    pub const fn listener(&self) -> &TcpListener {
        &self.listener
    }

    /// Returns the address and exposure class for host status reporting.
    #[must_use]
    pub const fn status(&self) -> ListenerStatus {
        self.status
    }
}

/// A redacted live-listener binding failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListenerBindError {
    Bind,
    NonLoopback,
}

impl core::fmt::Display for ListenerBindError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::Bind => "live listener bind failed",
            Self::NonLoopback => "live listener requires TLS for non-loopback exposure",
        })
    }
}

impl std::error::Error for ListenerBindError {}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 16 * 1024,
            max_header_bytes: 16 * 1024,
            max_frame_bytes: 16 * 1024 * 1024,
            lease_ms: 30_000,
            max_outgoing_bytes: 16 * 1024 * 1024,
            request_retention_ms: 30_000,
            tls: true,
        }
    }
}

impl TransportLimits {
    fn validate(self, protocol: ProtocolLimits) -> Result<Self> {
        if self.max_request_bytes == 0
            || self.max_header_bytes == 0
            || self.max_frame_bytes == 0
            || self.max_frame_bytes < protocol.max_message_bytes
            || self.lease_ms == 0
            || self.lease_ms > 300_000
            || self.max_outgoing_bytes == 0
            || self.request_retention_ms == 0
            || self.request_retention_ms < self.lease_ms
        {
            return Err(Error::Limit);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpEncodeError {
    Limit,
    Malformed,
    UnsupportedStatus,
}

impl WireResponse {
    /// Serializes one complete HTTP/1.1 response for a trusted socket edge.
    /// The encoder owns `Content-Length`, rejects header injection, and applies
    /// the configured outgoing bound before allocating the result buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when the response is malformed, uses an unsupported
    /// status, or exceeds the configured outgoing bound.
    pub fn encode_http(
        &self,
        limits: TransportLimits,
    ) -> std::result::Result<Vec<u8>, HttpEncodeError> {
        let reason = http_reason(self.status).ok_or(HttpEncodeError::UnsupportedStatus)?;
        let body_allowed = !(100..200).contains(&self.status) && self.status != 204;
        if !body_allowed && !self.body.is_empty() {
            return Err(HttpEncodeError::Malformed);
        }
        let mut length = 0usize;
        length = length
            .checked_add(9 + self.status.to_string().len() + reason.len() + 4)
            .ok_or(HttpEncodeError::Limit)?;
        let mut header_names = BTreeSet::new();
        for (name, value) in &self.headers {
            if name.is_empty()
                || !name.bytes().all(is_http_token)
                || value.bytes().any(is_http_header_control)
                || name.eq_ignore_ascii_case("content-length")
                || !header_names.insert(name.to_ascii_lowercase())
            {
                return Err(HttpEncodeError::Malformed);
            }
            length = length
                .checked_add(name.len() + value.len() + 4)
                .ok_or(HttpEncodeError::Limit)?;
        }
        if body_allowed {
            length = length
                .checked_add("Content-Length: ".len() + self.body.len().to_string().len() + 4)
                .and_then(|length| length.checked_add(self.body.len()))
                .ok_or(HttpEncodeError::Limit)?;
        }
        if length > limits.max_outgoing_bytes {
            return Err(HttpEncodeError::Limit);
        }

        let mut encoded = Vec::with_capacity(length);
        encoded.extend_from_slice(format!("HTTP/1.1 {} {}\r\n", self.status, reason).as_bytes());
        for (name, value) in &self.headers {
            encoded.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        if body_allowed {
            encoded.extend_from_slice(
                format!("Content-Length: {}\r\n\r\n", self.body.len()).as_bytes(),
            );
            encoded.extend_from_slice(&self.body);
        } else {
            encoded.extend_from_slice(b"\r\n");
        }
        Ok(encoded)
    }
}

const fn http_reason(status: u16) -> Option<&'static str> {
    Some(match status {
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        426 => "Upgrade Required",
        503 => "Service Unavailable",
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpParseError {
    Incomplete,
    Limit,
    Malformed,
}

impl core::fmt::Display for HttpParseError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::Incomplete => "HTTP request is incomplete",
            Self::Limit => "HTTP request exceeds the configured limit",
            Self::Malformed => "HTTP request is malformed",
        })
    }
}

impl std::error::Error for HttpParseError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedHttpRequest {
    request: WireRequest,
    consumed: usize,
}

impl ParsedHttpRequest {
    #[must_use]
    pub fn request(&self) -> &WireRequest {
        &self.request
    }

    #[must_use]
    pub const fn consumed(&self) -> usize {
        self.consumed
    }

    fn into_request(self) -> WireRequest {
        self.request
    }
}

/// Bounded connection-local HTTP read state. It retains incomplete bytes and
/// drains complete pipelined requests without exposing the backing buffer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpConnection {
    limits: TransportLimits,
    buffered: Vec<u8>,
    pending: VecDeque<ParsedHttpRequest>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpConnectionError {
    Parse(HttpParseError),
    Encode(HttpEncodeError),
    Protocol(Error),
    WebSocket(WebSocketEncodeError),
}

impl core::fmt::Display for HttpConnectionError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(formatter),
            Self::Encode(error) => formatter.write_str(match error {
                HttpEncodeError::Limit => "HTTP response exceeds the configured limit",
                HttpEncodeError::Malformed => "HTTP response is malformed",
                HttpEncodeError::UnsupportedStatus => "HTTP response status is unsupported",
            }),
            Self::Protocol(error) => error.fmt(formatter),
            Self::WebSocket(error) => formatter.write_str(match error {
                WebSocketEncodeError::Limit => "WebSocket output exceeds the configured limit",
            }),
        }
    }
}

impl std::error::Error for HttpConnectionError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpIoError {
    Read,
    Write,
    Cancelled,
    Transport(HttpConnectionError),
}

impl core::fmt::Display for HttpIoError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read => formatter.write_str("HTTP connection read failed"),
            Self::Write => formatter.write_str("HTTP connection write failed"),
            Self::Cancelled => formatter.write_str("HTTP connection cancelled"),
            Self::Transport(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for HttpIoError {}

async fn await_http_io<T, F, C>(
    operation: F,
    cancellation: &mut C,
    failure: HttpIoError,
) -> std::result::Result<T, HttpIoError>
where
    F: Future<Output = io::Result<T>> + Unpin,
    C: Future<Output = ()> + Unpin,
{
    let mut operation = operation;
    futures::future::poll_fn(|context| {
        if Pin::new(&mut *cancellation).poll(context).is_ready() {
            return std::task::Poll::Ready(Err(HttpIoError::Cancelled));
        }
        Pin::new(&mut operation)
            .poll(context)
            .map(|result| result.map_err(|_| failure))
    })
    .await
}

impl HttpConnection {
    #[must_use]
    pub fn new(limits: TransportLimits) -> Self {
        Self {
            limits,
            buffered: Vec::new(),
            pending: VecDeque::new(),
        }
    }

    /// Adds one listener read and returns every complete request available in
    /// it. The connection retains only one bounded incomplete request.
    ///
    /// # Errors
    ///
    /// Returns the parser's fail-closed framing or size error. On error, the
    /// connection state is unchanged by the rejected read.
    pub fn push(
        &mut self,
        bytes: &[u8],
    ) -> std::result::Result<Vec<ParsedHttpRequest>, HttpParseError> {
        let mut incoming = bytes;
        let mut parsed = Vec::new();
        let mut buffered = self.buffered.clone();
        while !incoming.is_empty() {
            if let Some(request) = parse_http_request(&buffered, self.limits)? {
                buffered.drain(..request.consumed());
                parsed.push(request);
                continue;
            }
            let available = self
                .limits
                .max_request_bytes
                .checked_sub(buffered.len())
                .ok_or(HttpParseError::Limit)?;
            if available == 0 {
                return Err(HttpParseError::Limit);
            }
            let take = incoming.len().min(available);
            buffered.extend_from_slice(&incoming[..take]);
            incoming = &incoming[take..];
        }
        while let Some(request) = parse_http_request(&buffered, self.limits)? {
            buffered.drain(..request.consumed());
            parsed.push(request);
        }
        self.buffered = buffered;
        Ok(parsed)
    }

    #[must_use]
    pub const fn buffered_bytes(&self) -> usize {
        self.buffered.len()
    }

    fn push_one(
        &mut self,
        bytes: &[u8],
    ) -> std::result::Result<Option<ParsedHttpRequest>, HttpParseError> {
        let parsed = self.push(bytes)?;
        self.pending.extend(parsed);
        Ok(self.pending.pop_front())
    }

    fn push_one_preserving_remainder(
        &mut self,
        bytes: &[u8],
    ) -> std::result::Result<Option<ParsedHttpRequest>, HttpParseError> {
        let mut candidate = self.buffered.clone();
        candidate.extend_from_slice(bytes);
        let Some(request) = parse_http_request(&candidate, self.limits)? else {
            if candidate.len() > self.limits.max_request_bytes {
                return Err(HttpParseError::Limit);
            }
            self.buffered = candidate;
            return Ok(None);
        };
        self.buffered = candidate.split_off(request.consumed());
        Ok(Some(request))
    }

    fn take_buffered(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffered)
    }
}

/// Decodes exactly one bounded HTTP/1.1 request from a listener read buffer.
/// The returned byte count lets a host retain pipelined bytes for a later
/// request; incomplete input is reported without consuming or interpreting it.
///
/// # Errors
///
/// Returns [`HttpParseError::Limit`] or [`HttpParseError::Malformed`] when the
/// buffer cannot be admitted under the bounded HTTP framing rules.
pub fn parse_http_request(
    bytes: &[u8],
    limits: TransportLimits,
) -> std::result::Result<Option<ParsedHttpRequest>, HttpParseError> {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return if bytes.len() > limits.max_header_bytes {
            Err(HttpParseError::Limit)
        } else {
            Ok(None)
        };
    };
    let header_length = header_end.checked_add(4).ok_or(HttpParseError::Limit)?;
    if header_length > limits.max_header_bytes {
        return Err(HttpParseError::Limit);
    }

    let header_text =
        core::str::from_utf8(&bytes[..header_end]).map_err(|_| HttpParseError::Malformed)?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or(HttpParseError::Malformed)?;
    let parts = request_line.split(' ').collect::<Vec<_>>();
    let [method, path, version] = parts.as_slice() else {
        return Err(HttpParseError::Malformed);
    };
    if !matches!(*method, "GET" | "POST" | "DELETE")
        || *version != "HTTP/1.1"
        || path.is_empty()
        || !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('?')
        || *path == "*"
        || !method.bytes().all(is_http_token)
        || path.bytes().any(is_http_control)
    {
        return Err(HttpParseError::Malformed);
    }

    let mut headers = Vec::new();
    let mut header_names = BTreeSet::new();
    let mut content_length = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(HttpParseError::Malformed)?;
        if name.is_empty()
            || !name.bytes().all(is_http_token)
            || value.bytes().any(is_http_header_control)
        {
            return Err(HttpParseError::Malformed);
        }
        if !header_names.insert(name.to_ascii_lowercase()) {
            return Err(HttpParseError::Malformed);
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(HttpParseError::Malformed);
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(HttpParseError::Malformed);
            }
            content_length = Some(value.parse::<usize>().map_err(|_| HttpParseError::Limit)?);
        }
        headers.push((name.to_owned(), value.to_owned()));
    }
    if !header_names.contains("host") {
        return Err(HttpParseError::Malformed);
    }

    let body_length = content_length.unwrap_or(0);
    let consumed = header_length
        .checked_add(body_length)
        .ok_or(HttpParseError::Limit)?;
    if consumed > limits.max_request_bytes {
        return Err(HttpParseError::Limit);
    }
    if bytes.len() < consumed {
        return Ok(None);
    }
    Ok(Some(ParsedHttpRequest {
        request: WireRequest {
            method: (*method).to_owned(),
            path: (*path).to_owned(),
            headers,
            body: bytes[header_length..consumed].to_vec(),
        },
        consumed,
    }))
}

const fn is_http_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

const fn is_http_control(byte: u8) -> bool {
    byte < 0x20 || byte == 0x7f
}

const fn is_http_header_control(byte: u8) -> bool {
    (byte < 0x20 && byte != b'\t') || byte == 0x7f
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebSocketOutput {
    Accepted(FrameOutcome),
    /// A canonical host response ready to send as one binary WebSocket frame.
    Binary {
        outcome: FrameOutcome,
        payload: Vec<u8>,
    },
    Pong(Vec<u8>),
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebSocketEncodeError {
    Limit,
}

/// Encodes one unfragmented, unmasked server-to-client RFC 6455 frame.
/// Logical acknowledgement-only outcomes produce no wire bytes.
///
/// # Errors
///
/// Returns [`WebSocketEncodeError::Limit`] when a payload exceeds the
/// effective outgoing bound or a control payload exceeds 125 bytes.
pub fn encode_websocket_output(
    output: &WebSocketOutput,
    limits: TransportLimits,
) -> std::result::Result<Option<Vec<u8>>, WebSocketEncodeError> {
    let (opcode, payload) = match output {
        WebSocketOutput::Accepted(_) => return Ok(None),
        WebSocketOutput::Binary { payload, .. } => (2, payload.as_slice()),
        WebSocketOutput::Pong(payload) => (10, payload.as_slice()),
        WebSocketOutput::Close => (8, &[] as &[u8]),
    };
    let control = matches!(opcode, 8..=10);
    if payload.len() > limits.max_frame_bytes || control && payload.len() > 125 {
        return Err(WebSocketEncodeError::Limit);
    }
    let (length_code, extended_length) = if payload.len() <= 125 {
        (u8::try_from(payload.len()).unwrap_or_default(), Vec::new())
    } else if u16::try_from(payload.len()).is_ok() {
        (
            126,
            u16::try_from(payload.len())
                .unwrap_or_default()
                .to_be_bytes()
                .to_vec(),
        )
    } else {
        (127, (payload.len() as u64).to_be_bytes().to_vec())
    };
    let total = 2 + extended_length.len() + payload.len();
    if total > limits.max_outgoing_bytes {
        return Err(WebSocketEncodeError::Limit);
    }
    let mut encoded = Vec::with_capacity(total);
    encoded.push(0x80 | opcode);
    encoded.push(length_code);
    encoded.extend_from_slice(&extended_length);
    encoded.extend_from_slice(payload);
    Ok(Some(encoded))
}

#[derive(Clone)]
struct SessionRecord {
    metadata: SessionMetadata,
    credential: SessionCredential,
}

struct UpgradeAdmission {
    id: [u8; 16],
    origin: Origin,
    credential: SessionCredential,
    attachment: [u8; 16],
    now: u64,
    response: WireResponse,
}

/// A validated WebSocket handshake waiting for the host to deliver its 101
/// response. The attachment is committed only after that delivery succeeds.
pub struct WebSocketUpgrade {
    admission: UpgradeAdmission,
}

impl WebSocketUpgrade {
    #[must_use]
    pub const fn response(&self) -> &WireResponse {
        &self.admission.response
    }
}

/// A bounded HTTP/RFC-6455 parser and adapter. It deliberately has no listener,
/// socket, clock, TLS or authentication implementation; those stay at the
/// executable host edge.
pub struct LiveTransport {
    host: LiveHost,
    limits: TransportLimits,
    sessions: BTreeMap<[u8; 16], SessionRecord>,
    retired_attachments: VecDeque<[u8; 16]>,
}

impl LiveTransport {
    /// Binds the default live listener to IPv4 loopback only.
    ///
    /// # Errors
    ///
    /// Returns a redacted bind error when the loopback listener cannot be
    /// created.
    pub fn bind_default_listener(
        port: u16,
    ) -> std::result::Result<LiveListener, ListenerBindError> {
        Self::bind_listener(
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            ListenerExposure::Loopback,
        )
    }

    /// Binds a loopback listener at an address explicitly selected by the
    /// operator. Non-loopback addresses fail before binding because this
    /// transport boundary does not own TLS.
    ///
    /// # Errors
    ///
    /// Returns a redacted bind error when the requested listener is not
    /// loopback or cannot be created.
    pub fn bind_explicit_listener(
        address: SocketAddr,
    ) -> std::result::Result<LiveListener, ListenerBindError> {
        if !address.ip().is_loopback() {
            return Err(ListenerBindError::NonLoopback);
        }
        Self::bind_listener(address, ListenerExposure::Explicit)
    }

    fn bind_listener(
        address: SocketAddr,
        exposure: ListenerExposure,
    ) -> std::result::Result<LiveListener, ListenerBindError> {
        let listener = TcpListener::bind(address).map_err(|_| ListenerBindError::Bind)?;
        let address = listener.local_addr().map_err(|_| ListenerBindError::Bind)?;
        Ok(LiveListener {
            listener,
            status: ListenerStatus { address, exposure },
        })
    }

    /// Accepts one stream through a caller-supplied runtime adapter.
    ///
    /// This only races acceptance with cancellation. The caller retains TLS,
    /// connection lifetime, attachment identity, and subsequent HTTP or
    /// WebSocket handoff ownership; no production listener loop is implied.
    ///
    /// # Errors
    ///
    /// Returns a redacted accept or cancellation error.
    pub async fn accept_listener_with_cancellation<A, C>(
        &self,
        listener: &LiveListener,
        acceptor: &mut A,
        cancellation: &mut C,
    ) -> std::result::Result<std::net::TcpStream, HttpIoError>
    where
        A: LiveListenerAcceptor,
        C: Future<Output = ()> + Unpin,
    {
        await_http_io(
            acceptor.accept(listener.listener()),
            cancellation,
            HttpIoError::Read,
        )
        .await
    }

    /// # Errors
    ///
    /// Returns [`Error::Limit`] for an invalid transport bound.
    pub fn new(host: LiveHost, limits: TransportLimits) -> Result<Self> {
        Ok(Self {
            limits: limits.validate(host.limits.protocol)?,
            host,
            sessions: BTreeMap::new(),
            retired_attachments: VecDeque::new(),
        })
    }

    /// Takes attachment identities retired by successful HTTP session resume.
    /// The executable host uses these identities to cancel and join the old
    /// socket task without changing session-owned work.
    pub fn take_retired_attachments(&mut self) -> Vec<[u8; 16]> {
        self.retired_attachments.drain(..).collect()
    }

    /// Idempotently closes one attachment through the transport-owned host
    /// state. Retired workers may call this after replacement; the inner host
    /// reports [`Error::Closed`] without affecting the replacement.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Closed`] when the attachment has already been
    /// retired or otherwise left the host.
    pub async fn close_attachment(
        &mut self,
        attachment: [u8; 16],
        now: u64,
    ) -> Result<FrameOutcome> {
        self.host.close_attachment(attachment, now).await
    }

    /// Parses exactly the three live-session HTTP endpoint shapes.
    #[allow(clippy::too_many_lines)]
    pub async fn handle(
        &mut self,
        request: WireRequest,
        now: u64,
        authority: &mut impl LiveSessionAuthority,
        issuer: &mut impl LiveCredentialIssuer,
        deletion: &mut impl DeletionAdapter,
    ) -> WireResponse {
        if request.body.len() > self.limits.max_request_bytes
            || header_size(&request.headers) > self.limits.max_header_bytes
        {
            return wire_error(413, "live.limit");
        }
        let Ok(origin) = required_origin(&request.headers) else {
            return wire_error(403, "live.origin_denied");
        };
        if request.method == "POST"
            && !header_eq(&request.headers, "content-type", "application/json")
        {
            return wire_error(400, "live.malformed_request");
        }
        match (request.method.as_str(), session_path(&request.path)) {
            ("POST", SessionPath::Create) => {
                let Some((database, protocol)) =
                    parse_json_pair(&request.body, "database", "protocol")
                else {
                    return wire_error(400, "live.malformed_request");
                };
                if protocol != SUBPROTOCOL {
                    return wire_error(409, "live.incompatible");
                }
                let Some(database) = parse_uuid(&database) else {
                    return wire_error(400, "live.malformed_request");
                };
                let metadata = match authority.create_session(database, now) {
                    Ok(metadata) if metadata.database == database && metadata.expires_at > now => {
                        metadata
                    }
                    Err(Error::Denied) => return wire_error(404, "live.database_unavailable"),
                    Ok(_) | Err(_) => return wire_error(503, "live.unavailable"),
                };
                let credential = match self
                    .host
                    .create(
                        CreateRequest {
                            id: metadata.session,
                            origin,
                            expires_at: metadata.expires_at,
                            now,
                            subscribe: &metadata.subscribe,
                        },
                        issuer,
                    )
                    .await
                {
                    Ok(credential) => credential,
                    Err(error) => return host_error(error),
                };
                let Some(token) = issuer.last_issued() else {
                    return wire_error(503, "live.unavailable");
                };
                let record = SessionRecord {
                    metadata,
                    credential,
                };
                let response = session_response(&record.metadata, token, self.limits, 201);
                self.sessions.insert(record.metadata.session, record);
                response
            }
            ("POST", SessionPath::Resume(id)) => {
                let Some((token, protocol)) =
                    parse_json_pair(&request.body, "resume_token", "protocol")
                else {
                    return wire_error(400, "live.malformed_request");
                };
                if protocol != SUBPROTOCOL {
                    return wire_error(409, "live.incompatible");
                }
                let Some(token) = decode_token(&token) else {
                    return wire_error(400, "live.malformed_request");
                };
                let Some(record) = self.sessions.get_mut(&id) else {
                    return wire_error(410, "live.expired");
                };
                let supplied = SessionCredential {
                    security: OpaqueCredential::from_bytes(token),
                    serving: ServingCredential::new(token),
                };
                match self
                    .host
                    .rotate_and_retire(id, &origin, &supplied, now, issuer)
                    .await
                {
                    Ok((credential, retired)) => {
                        if let Some(attachment) = retired {
                            self.retired_attachments.push_back(attachment);
                        }
                        let Some(replacement) = issuer.last_issued() else {
                            return wire_error(503, "live.unavailable");
                        };
                        record.credential = credential;
                        session_response(&record.metadata, replacement, self.limits, 200)
                    }
                    Err(Error::Closed | Error::Denied) => wire_error(410, "live.expired"),
                    Err(error) => host_error(error),
                }
            }
            ("DELETE", SessionPath::Session(id)) => {
                if !request.body.is_empty() {
                    return wire_error(400, "live.malformed_request");
                }
                let Some(token) = bearer(&request.headers).and_then(|value| decode_token(&value))
                else {
                    return wire_error(401, "live.unauthenticated");
                };
                let credential = SessionCredential {
                    security: OpaqueCredential::from_bytes(token),
                    serving: ServingCredential::new(token),
                };
                let authorised = self
                    .sessions
                    .get(&id)
                    .is_some_and(|record| record.credential == credential);
                if !authorised {
                    return wire_error(410, "live.expired");
                }
                match self.host.delete(DeleteRequest { id }, deletion).await {
                    Ok(()) => {
                        self.sessions.remove(&id);
                        WireResponse {
                            status: 204,
                            headers: Vec::new(),
                            body: Vec::new(),
                        }
                    }
                    Err(error) => host_error(error),
                }
            }
            _ => wire_error(400, "live.malformed_request"),
        }
    }

    /// Routes complete HTTP requests from a bounded connection and serializes
    /// each response for the listener write side. WebSocket upgrade requests
    /// remain explicit through [`Self::upgrade`].
    ///
    /// # Errors
    ///
    /// Returns a fail-closed parser or response-encoding error. Routed HTTP
    /// failures are encoded as ordinary protocol responses in the result.
    pub async fn handle_http_read(
        &mut self,
        connection: &mut HttpConnection,
        bytes: &[u8],
        now: u64,
        authority: &mut impl LiveSessionAuthority,
        issuer: &mut impl LiveCredentialIssuer,
        deletion: &mut impl DeletionAdapter,
    ) -> std::result::Result<Vec<Vec<u8>>, HttpConnectionError> {
        let requests = connection.push(bytes).map_err(HttpConnectionError::Parse)?;
        let mut responses = Vec::with_capacity(requests.len());
        for request in requests {
            let response = self
                .handle(request.into_request(), now, authority, issuer, deletion)
                .await;
            responses.push(
                response
                    .encode_http(self.limits)
                    .map_err(HttpConnectionError::Encode)?,
            );
        }
        Ok(responses)
    }

    /// Serves the HTTP session lifecycle over an injected byte stream until
    /// EOF. Binding, TLS, clock ownership, and WebSocket handoff stay with the
    /// executable host; this loop only reads, routes, and writes bounded HTTP.
    ///
    /// # Errors
    ///
    /// Returns a redacted read, write, parser, or response-encoding error.
    #[allow(clippy::too_many_arguments)]
    pub async fn serve_http_connection<R, W>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        connection: &mut HttpConnection,
        now: u64,
        authority: &mut impl LiveSessionAuthority,
        issuer: &mut impl LiveCredentialIssuer,
        deletion: &mut impl DeletionAdapter,
    ) -> std::result::Result<(), HttpIoError>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut clock = || now;
        self.serve_http_connection_with_clock(
            reader, writer, connection, &mut clock, authority, issuer, deletion,
        )
        .await
    }

    /// Serves an injected HTTP byte stream while taking a fresh timestamp for
    /// every routed request. Complete requests are admitted and written one at
    /// a time, so a failed write cannot cause later pipelined requests to run.
    ///
    /// # Errors
    ///
    /// Returns a redacted read, write, parser, or response-encoding error, or
    /// an incomplete-request error when EOF arrives with retained bytes.
    #[allow(clippy::too_many_arguments)]
    pub async fn serve_http_connection_with_clock<R, W, C>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        connection: &mut HttpConnection,
        clock: &mut C,
        authority: &mut impl LiveSessionAuthority,
        issuer: &mut impl LiveCredentialIssuer,
        deletion: &mut impl DeletionAdapter,
    ) -> std::result::Result<(), HttpIoError>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
        C: FnMut() -> u64,
    {
        let mut cancellation = std::future::pending::<()>();
        self.serve_http_connection_with_cancellation(
            reader,
            writer,
            connection,
            clock,
            &mut cancellation,
            authority,
            issuer,
            deletion,
        )
        .await
    }

    /// Drives the bounded HTTP session loop over one already-accepted TCP
    /// stream. The executable host retains bind, accept, TLS, cancellation,
    /// and clock ownership; this bridge only adapts the accepted socket to the
    /// transport's byte-stream contract.
    ///
    /// # Errors
    ///
    /// Returns a redacted socket-cloning, read, write, parser, or
    /// response-encoding error. This synchronous bridge blocks the calling
    /// thread while the peer is connected.
    #[allow(clippy::too_many_arguments)]
    pub fn serve_accepted_http_socket<C>(
        &mut self,
        stream: std::net::TcpStream,
        connection: &mut HttpConnection,
        clock: &mut C,
        authority: &mut impl LiveSessionAuthority,
        issuer: &mut impl LiveCredentialIssuer,
        deletion: &mut impl DeletionAdapter,
    ) -> std::result::Result<(), HttpIoError>
    where
        C: FnMut() -> u64,
    {
        let reader = stream.try_clone().map_err(|_| HttpIoError::Read)?;
        let mut reader = AllowStdIo::new(reader);
        let mut writer = AllowStdIo::new(stream);
        let mut cancellation = std::future::pending::<()>();
        block_on(self.serve_http_connection_with_cancellation(
            &mut reader,
            &mut writer,
            connection,
            clock,
            &mut cancellation,
            authority,
            issuer,
            deletion,
        ))
    }

    /// Accepts and serves exactly one HTTP connection from a caller-owned
    /// listener. Binding, listener lifetime, TLS, cancellation, and clock
    /// ownership remain with the executable host. This is a bounded
    /// synchronous handoff, not a production listener.
    ///
    /// # Errors
    ///
    /// Returns a redacted accept, socket-cloning, read, write, parser, or
    /// response-encoding error. This function blocks until one peer connects
    /// and then while that peer remains connected.
    #[allow(clippy::too_many_arguments)]
    pub fn serve_one_http_listener<C>(
        &mut self,
        listener: &std::net::TcpListener,
        connection: &mut HttpConnection,
        clock: &mut C,
        authority: &mut impl LiveSessionAuthority,
        issuer: &mut impl LiveCredentialIssuer,
        deletion: &mut impl DeletionAdapter,
    ) -> std::result::Result<(), HttpIoError>
    where
        C: FnMut() -> u64,
    {
        let (stream, _) = listener.accept().map_err(|_| HttpIoError::Read)?;
        self.serve_accepted_http_socket(stream, connection, clock, authority, issuer, deletion)
    }

    /// Drives one already-accepted TCP stream through the bounded HTTP
    /// WebSocket upgrade and frame loop. The executable host retains bind,
    /// accept, TLS, cancellation, attachment identity, and clock ownership;
    /// this bridge only adapts the accepted socket to the transport's
    /// byte-stream contract.
    ///
    /// # Errors
    ///
    /// Returns a redacted socket-cloning, read, write, parser, framing,
    /// protocol, or response-encoding error. This synchronous bridge blocks
    /// the calling thread while the peer is connected.
    #[allow(clippy::too_many_arguments)]
    pub fn serve_accepted_websocket_socket<C, A>(
        &mut self,
        stream: std::net::TcpStream,
        connection: &mut HttpConnection,
        attachment: [u8; 16],
        clock: &mut C,
        application: &mut A,
    ) -> std::result::Result<(), HttpIoError>
    where
        C: FnMut() -> u64,
        A: LiveApplication,
    {
        let reader = stream.try_clone().map_err(|_| HttpIoError::Read)?;
        let mut reader = AllowStdIo::new(reader);
        let mut writer = AllowStdIo::new(stream);
        let mut cancellation = std::future::pending::<()>();
        block_on(self.serve_websocket_connection(
            &mut reader,
            &mut writer,
            connection,
            attachment,
            clock,
            &mut cancellation,
            application,
        ))
    }

    /// Accepts and serves exactly one WebSocket connection from a
    /// caller-owned listener. Binding, listener lifetime, TLS, cancellation,
    /// attachment identity, and clock ownership remain with the executable
    /// host. This is a bounded synchronous handoff, not a production listener.
    ///
    /// # Errors
    ///
    /// Returns a redacted accept, socket-cloning, read, write, parser,
    /// framing, protocol, or response-encoding error. This function blocks the
    /// calling thread until one peer connects and then while that peer remains
    /// connected.
    #[allow(clippy::too_many_arguments)]
    pub fn serve_one_websocket_listener<C, A>(
        &mut self,
        listener: &std::net::TcpListener,
        connection: &mut HttpConnection,
        attachment: [u8; 16],
        clock: &mut C,
        application: &mut A,
    ) -> std::result::Result<(), HttpIoError>
    where
        C: FnMut() -> u64,
        A: LiveApplication,
    {
        let (stream, _) = listener.accept().map_err(|_| HttpIoError::Read)?;
        self.serve_accepted_websocket_socket(stream, connection, attachment, clock, application)
    }

    /// Serves an injected HTTP byte stream with cancellation raced against
    /// every read, write, and flush. The cancellation future must wake the
    /// task when it becomes ready so a stalled peer cannot hold the task.
    /// Cancellation may occur after a partial response write; callers must
    /// dispose of the reader and writer and must not reuse this connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted read, write, cancellation, parser, or response-
    /// encoding error, or an incomplete-request error at EOF.
    #[allow(clippy::too_many_arguments)]
    pub async fn serve_http_connection_with_cancellation<R, W, C, X>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        connection: &mut HttpConnection,
        clock: &mut C,
        cancellation: &mut X,
        authority: &mut impl LiveSessionAuthority,
        issuer: &mut impl LiveCredentialIssuer,
        deletion: &mut impl DeletionAdapter,
    ) -> std::result::Result<(), HttpIoError>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
        C: FnMut() -> u64,
        X: Future<Output = ()> + Unpin,
    {
        let mut chunk = [0; 8192];
        loop {
            let read =
                await_http_io(reader.read(&mut chunk), cancellation, HttpIoError::Read).await?;
            if read == 0 {
                return if connection.buffered_bytes() == 0 {
                    Ok(())
                } else {
                    Err(HttpIoError::Transport(HttpConnectionError::Parse(
                        HttpParseError::Incomplete,
                    )))
                };
            }
            let mut next_request = connection
                .push_one(&chunk[..read])
                .map_err(HttpConnectionError::Parse)
                .map_err(HttpIoError::Transport)?;
            while let Some(request) = next_request {
                let response = self
                    .handle(request.into_request(), clock(), authority, issuer, deletion)
                    .await
                    .encode_http(self.limits)
                    .map_err(HttpConnectionError::Encode)
                    .map_err(HttpIoError::Transport)?;
                await_http_io(
                    writer.write_all(&response),
                    cancellation,
                    HttpIoError::Write,
                )
                .await?;
                await_http_io(writer.flush(), cancellation, HttpIoError::Write).await?;
                next_request = connection
                    .push_one(&[])
                    .map_err(HttpConnectionError::Parse)
                    .map_err(HttpIoError::Transport)?;
            }
        }
    }

    /// Performs one injected HTTP upgrade and continues on the same byte
    /// stream as WebSocket. Bytes co-read after the HTTP headers are handed to
    /// the WebSocket state machine; the 101 response is written before any
    /// application output is emitted.
    ///
    /// # Errors
    ///
    /// Returns a redacted read, write, cancellation, framing, protocol, or
    /// WebSocket-output error. After cancellation or a partial write, callers
    /// must dispose of the stream.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub async fn serve_websocket_connection<R, W, C, X, A>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        connection: &mut HttpConnection,
        attachment: [u8; 16],
        clock: &mut C,
        cancellation: &mut X,
        application: &mut A,
    ) -> std::result::Result<(), HttpIoError>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
        C: FnMut() -> u64,
        X: Future<Output = ()> + Unpin,
        A: LiveApplication,
    {
        let mut chunk = [0; 8192];
        let request = loop {
            let read =
                await_http_io(reader.read(&mut chunk), cancellation, HttpIoError::Read).await?;
            if read == 0 {
                return if connection.buffered_bytes() == 0 {
                    Ok(())
                } else {
                    Err(HttpIoError::Transport(HttpConnectionError::Parse(
                        HttpParseError::Incomplete,
                    )))
                };
            }
            if let Some(request) = connection
                .push_one_preserving_remainder(&chunk[..read])
                .map_err(HttpConnectionError::Parse)
                .map_err(HttpIoError::Transport)?
            {
                break request;
            }
        };
        let now = clock();
        let admission = match self.prepare_upgrade(request.request(), attachment, now) {
            Ok(admission) => admission,
            Err(response) => {
                let encoded = response
                    .encode_http(self.limits)
                    .map_err(HttpConnectionError::Encode)
                    .map_err(HttpIoError::Transport)?;
                await_http_io(writer.write_all(&encoded), cancellation, HttpIoError::Write).await?;
                await_http_io(writer.flush(), cancellation, HttpIoError::Write).await?;
                return Ok(());
            }
        };
        let response = admission.response.clone();
        let encoded = response
            .encode_http(self.limits)
            .map_err(HttpConnectionError::Encode)
            .map_err(HttpIoError::Transport)?;
        await_http_io(writer.write_all(&encoded), cancellation, HttpIoError::Write).await?;
        await_http_io(writer.flush(), cancellation, HttpIoError::Write).await?;
        if response.status != 101 {
            return Ok(());
        }
        self.commit_upgrade(admission)
            .await
            .map_err(HttpConnectionError::Protocol)
            .map_err(HttpIoError::Transport)?;

        let mut socket = WebSocketState::new(attachment);
        let mut initial = connection.take_buffered();
        loop {
            if initial.is_empty() {
                let read =
                    match await_http_io(reader.read(&mut chunk), cancellation, HttpIoError::Read)
                        .await
                    {
                        Ok(read) => read,
                        Err(error) => {
                            self.close_websocket_attachment(
                                socket.attachment,
                                clock(),
                                application,
                            )
                            .await?;
                            return Err(error);
                        }
                    };
                if read == 0 {
                    self.close_websocket_attachment(socket.attachment, clock(), application)
                        .await?;
                    return Ok(());
                }
                match self
                    .serve_websocket_bytes(
                        &mut socket,
                        &chunk[..read],
                        clock,
                        writer,
                        cancellation,
                        application,
                    )
                    .await
                {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => {
                        self.close_websocket_attachment(socket.attachment, clock(), application)
                            .await?;
                        return Err(error);
                    }
                }
            } else {
                let bytes = std::mem::take(&mut initial);
                match self
                    .serve_websocket_bytes(
                        &mut socket,
                        &bytes,
                        clock,
                        writer,
                        cancellation,
                        application,
                    )
                    .await
                {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => {
                        self.close_websocket_attachment(socket.attachment, clock(), application)
                            .await?;
                        return Err(error);
                    }
                }
            }
        }
    }

    async fn close_websocket_attachment<A>(
        &mut self,
        attachment: [u8; 16],
        now: u64,
        application: &mut A,
    ) -> std::result::Result<(), HttpIoError>
    where
        A: LiveApplication,
    {
        self.host
            .dispatch_frame(attachment, now, Frame::Close, application)
            .await
            .map_err(HttpConnectionError::Protocol)
            .map_err(HttpIoError::Transport)?;
        Ok(())
    }

    /// Validates the RFC 6455 handshake and attaches the cookie-authenticated
    /// session. The caller supplies its socket identity after accepting bytes.
    pub async fn upgrade(
        &mut self,
        request: WireRequest,
        attachment: [u8; 16],
        now: u64,
    ) -> WireResponse {
        let admission = match self.prepare_upgrade(&request, attachment, now) {
            Ok(admission) => admission,
            Err(response) => return response,
        };
        match self.commit_upgrade(admission).await {
            Ok(response) => response,
            Err(error) => host_error(error),
        }
    }

    /// Validates a WebSocket handshake without changing session state.
    ///
    /// The returned value must be committed after the host has successfully
    /// delivered its 101 response. This keeps failed socket writes from
    /// creating an attachment that has no live owner.
    ///
    /// # Errors
    ///
    /// Returns the redacted handshake response when validation fails.
    pub fn prepare_websocket_upgrade(
        &self,
        request: &WireRequest,
        attachment: [u8; 16],
        now: u64,
    ) -> std::result::Result<WebSocketUpgrade, WireResponse> {
        self.prepare_upgrade(request, attachment, now)
            .map(|admission| WebSocketUpgrade { admission })
    }

    /// Commits a previously validated handshake after its 101 response was
    /// delivered to the peer.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol error when the session cannot be attached.
    pub async fn commit_websocket_upgrade(
        &mut self,
        upgrade: WebSocketUpgrade,
    ) -> Result<WireResponse> {
        self.commit_upgrade(upgrade.admission).await
    }

    fn prepare_upgrade(
        &self,
        request: &WireRequest,
        attachment: [u8; 16],
        now: u64,
    ) -> std::result::Result<UpgradeAdmission, WireResponse> {
        if !request.body.is_empty() || header_size(&request.headers) > self.limits.max_header_bytes
        {
            return Err(wire_error(400, "live.malformed_request"));
        }
        let SessionPath::Live(id) = session_path(&request.path) else {
            return Err(wire_error(400, "live.malformed_request"));
        };
        let Ok(origin) = required_origin(&request.headers) else {
            return Err(wire_error(403, "live.origin_denied"));
        };
        let Some(record) = self.sessions.get(&id) else {
            return Err(wire_error(410, "live.expired"));
        };
        let Some(cookie) =
            cookie(&request.headers, "orna_session").and_then(|value| decode_token(&value))
        else {
            return Err(wire_error(401, "live.unauthenticated"));
        };
        if request.method != "GET"
            || !header_token(&request.headers, "connection", "upgrade")
            || !header_eq(&request.headers, "upgrade", "websocket")
            || !header_eq(&request.headers, "sec-websocket-version", "13")
            || !header_token(&request.headers, "sec-websocket-protocol", SUBPROTOCOL)
        {
            return Err(wire_error(400, "live.malformed_request"));
        }
        let Some(key) = header(&request.headers, "sec-websocket-key").and_then(decode_base64)
        else {
            return Err(wire_error(400, "live.malformed_request"));
        };
        if key.len() != 16 {
            return Err(wire_error(400, "live.malformed_request"));
        }
        let credential = SessionCredential {
            security: OpaqueCredential::from_bytes(cookie),
            serving: ServingCredential::new(cookie),
        };
        if credential != record.credential {
            return Err(wire_error(401, "live.unauthenticated"));
        }
        self.host
            .validate_resume(&ResumeRequest {
                id,
                origin: &origin,
                credential: &credential,
                attachment,
                now,
            })
            .map_err(|error| match error {
                Error::Closed | Error::Denied => wire_error(410, "live.expired"),
                error => host_error(error),
            })?;
        Ok(UpgradeAdmission {
            id,
            origin,
            credential,
            attachment,
            now,
            response: WireResponse {
                status: 101,
                headers: vec![
                    ("connection".into(), "Upgrade".into()),
                    ("upgrade".into(), "websocket".into()),
                    (
                        "sec-websocket-accept".into(),
                        websocket_accept(
                            header(&request.headers, "sec-websocket-key").unwrap_or_default(),
                        ),
                    ),
                    ("sec-websocket-protocol".into(), SUBPROTOCOL.into()),
                ],
                body: Vec::new(),
            },
        })
    }

    async fn commit_upgrade(&mut self, admission: UpgradeAdmission) -> Result<WireResponse> {
        let outcome = self
            .host
            .resume(ResumeRequest {
                id: admission.id,
                origin: &admission.origin,
                credential: &admission.credential,
                attachment: admission.attachment,
                now: admission.now,
            })
            .await?;
        if let AttachOutcome::Replaced(previous) = outcome {
            self.retired_attachments.push_back(previous.as_bytes());
        }
        Ok(admission.response)
    }

    /// Reassembles client RFC 6455 frames and forwards only complete binary
    /// canonical envelopes into [`LiveHost`].
    ///
    /// # Errors
    ///
    /// Returns a redacted error for malformed, unmasked, oversized, text, or
    /// noncanonical application input; no partial message is forwarded.
    pub async fn receive(
        &mut self,
        socket: &mut WebSocketState,
        now: u64,
        bytes: &[u8],
    ) -> Result<Vec<WebSocketOutput>> {
        let mut application = RejectApplication;
        self.receive_with_application(socket, now, bytes, &mut application)
            .await
    }

    /// Reassembles and dispatches complete application messages through the
    /// supplied runtime/presentation host. The compatibility [`Self::receive`]
    /// method remains fail-closed for callers that have not supplied one.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol, size, transport, or application-boundary
    /// error without forwarding a partial message.
    pub async fn receive_with_application(
        &mut self,
        socket: &mut WebSocketState,
        now: u64,
        bytes: &[u8],
        application: &mut impl LiveApplication,
    ) -> Result<Vec<WebSocketOutput>> {
        let mut output = Vec::new();
        let mut input = bytes;
        loop {
            if let Some(next) = self
                .receive_one_with_application(socket, now, input, application)
                .await?
            {
                output.push(next);
                if socket.closed {
                    output.push(WebSocketOutput::Close);
                }
            }
            input = &[];
            if !socket.has_complete_frame(self.limits.max_frame_bytes)? {
                break;
            }
        }
        Ok(output)
    }

    async fn receive_one_with_application(
        &mut self,
        socket: &mut WebSocketState,
        now: u64,
        bytes: &[u8],
        application: &mut impl LiveApplication,
    ) -> Result<Option<WebSocketOutput>> {
        let Some(event) = socket.push_one(bytes, self.limits.max_frame_bytes)? else {
            return Ok(None);
        };
        match event {
            SocketEvent::Binary(message) => {
                let dispatched = self
                    .host
                    .dispatch_frame(socket.attachment, now, Frame::Binary(message), application)
                    .await?;
                Ok(Some(self.websocket_output(dispatched)?))
            }
            SocketEvent::Text => Err(Error::InvalidFrame),
            SocketEvent::Ping(payload) => Ok(Some(WebSocketOutput::Pong(payload))),
            SocketEvent::Pong => Ok(None),
            SocketEvent::Close => {
                let outcome = self
                    .host
                    .dispatch_frame(socket.attachment, now, Frame::Close, application)
                    .await?
                    .outcome;
                Ok(Some(WebSocketOutput::Accepted(outcome)))
            }
        }
    }

    async fn serve_websocket_bytes<W, C, X, A>(
        &mut self,
        socket: &mut WebSocketState,
        bytes: &[u8],
        clock: &mut C,
        writer: &mut W,
        cancellation: &mut X,
        application: &mut A,
    ) -> std::result::Result<bool, HttpIoError>
    where
        W: AsyncWrite + Unpin,
        C: FnMut() -> u64,
        X: Future<Output = ()> + Unpin,
        A: LiveApplication,
    {
        let mut input = bytes;
        loop {
            let output = self
                .receive_one_with_application(socket, clock(), input, application)
                .await
                .map_err(HttpConnectionError::Protocol)
                .map_err(HttpIoError::Transport)?;
            if let Some(output) = output {
                let closed = socket.closed;
                if write_websocket_outputs(self.limits, writer, cancellation, vec![output]).await? {
                    return Ok(true);
                }
                if closed {
                    write_websocket_outputs(
                        self.limits,
                        writer,
                        cancellation,
                        vec![WebSocketOutput::Close],
                    )
                    .await?;
                    return Ok(true);
                }
            }
            input = &[];
            if !socket
                .has_complete_frame(self.limits.max_frame_bytes)
                .map_err(HttpConnectionError::Protocol)
                .map_err(HttpIoError::Transport)?
            {
                return Ok(false);
            }
        }
    }

    fn websocket_output(&self, dispatched: DispatchOutcome) -> Result<WebSocketOutput> {
        let DispatchOutcome { outcome, response } = dispatched;
        let Some(response) = response else {
            return Ok(WebSocketOutput::Accepted(outcome));
        };
        let payload = response
            .encode(self.host.limits.protocol)
            .map_err(|_| Error::ApplicationRejected)?;
        Ok(WebSocketOutput::Binary { outcome, payload })
    }
}

async fn write_websocket_outputs<W, X>(
    limits: TransportLimits,
    writer: &mut W,
    cancellation: &mut X,
    outputs: Vec<WebSocketOutput>,
) -> std::result::Result<bool, HttpIoError>
where
    W: AsyncWrite + Unpin,
    X: Future<Output = ()> + Unpin,
{
    for output in outputs {
        let closing = matches!(output, WebSocketOutput::Close);
        let Some(frame) = encode_websocket_output(&output, limits)
            .map_err(HttpConnectionError::WebSocket)
            .map_err(HttpIoError::Transport)?
        else {
            continue;
        };
        await_http_io(writer.write_all(&frame), cancellation, HttpIoError::Write).await?;
        await_http_io(writer.flush(), cancellation, HttpIoError::Write).await?;
        if closing {
            await_http_io(writer.close(), cancellation, HttpIoError::Write).await?;
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionPath {
    Create,
    Session([u8; 16]),
    Resume([u8; 16]),
    Live([u8; 16]),
    Invalid,
}
fn session_path(path: &str) -> SessionPath {
    if path == "/orna/session" {
        return SessionPath::Create;
    }
    let Some(rest) = path.strip_prefix("/orna/session/") else {
        return path
            .strip_prefix("/orna/live/")
            .and_then(parse_uuid)
            .map_or(SessionPath::Invalid, SessionPath::Live);
    };
    if let Some(id) = rest.strip_suffix("/resume").and_then(parse_uuid) {
        SessionPath::Resume(id)
    } else {
        parse_uuid(rest).map_or(SessionPath::Invalid, SessionPath::Session)
    }
}
fn parse_uuid(value: &str) -> Option<[u8; 16]> {
    if value.len() != 36
        || ![8, 13, 18, 23]
            .into_iter()
            .all(|index| value.as_bytes()[index] == b'-')
    {
        return None;
    }
    let mut out = [0; 16];
    let mut chars = value.bytes().filter(|byte| *byte != b'-');
    for target in &mut out {
        *target = (hex(chars.next()?)? << 4) | hex(chars.next()?)?;
    }
    chars.next().is_none().then_some(out)
}
const fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
fn uuid(value: [u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    for (index, byte) in value.into_iter().enumerate() {
        if [4, 6, 8, 10].contains(&index) {
            out.push('-');
        }
        out.push(nibble(byte >> 4));
        out.push(nibble(byte & 15));
    }
    out
}
const fn nibble(value: u8) -> char {
    b"0123456789abcdef"[value as usize] as char
}
fn header_size(headers: &[(String, String)]) -> usize {
    headers
        .iter()
        .map(|(name, value)| name.len().saturating_add(value.len()).saturating_add(4))
        .sum()
}
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let mut found = None;
    for (candidate, value) in headers {
        if candidate.eq_ignore_ascii_case(name) && found.replace(value.as_str()).is_some() {
            return None;
        }
    }
    found
}
fn header_eq(headers: &[(String, String)], name: &str, expected: &str) -> bool {
    header(headers, name).is_some_and(|value| value.eq_ignore_ascii_case(expected))
}
fn header_token(headers: &[(String, String)], name: &str, expected: &str) -> bool {
    header(headers, name).is_some_and(|value| {
        value
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case(expected))
    })
}
fn required_origin(headers: &[(String, String)]) -> std::result::Result<Origin, ()> {
    header(headers, "origin")
        .ok_or(())
        .and_then(|value| Origin::parse(value).map_err(|_| ()))
}
fn bearer(headers: &[(String, String)]) -> Option<String> {
    header(headers, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty() && !value.contains(char::is_whitespace))
        .map(str::to_owned)
}
fn cookie(headers: &[(String, String)], name: &str) -> Option<String> {
    let mut found = None;
    for part in header(headers, "cookie")?.split(';') {
        let (key, value) = part.trim().split_once('=')?;
        if key == name && found.replace(value).is_some() {
            return None;
        }
    }
    found.map(str::to_owned)
}
fn parse_json_pair(bytes: &[u8], first: &str, second: &str) -> Option<(String, String)> {
    let text = core::str::from_utf8(bytes).ok()?;
    let mut input = text.trim();
    input = input.strip_prefix('{')?.trim_start();
    let mut values = BTreeMap::new();
    while !input.starts_with('}') {
        let (key, rest) = json_string(input)?;
        input = rest.trim_start();
        input = input.strip_prefix(':')?.trim_start();
        let (value, rest) = json_string(input)?;
        if values.insert(key, value).is_some() {
            return None;
        }
        input = rest.trim_start();
        if input.starts_with(',') {
            input = input[1..].trim_start();
        } else {
            break;
        }
    }
    if input.strip_prefix('}')?.trim().is_empty() && values.len() == 2 {
        Some((values.remove(first)?, values.remove(second)?))
    } else {
        None
    }
}
fn json_string(input: &str) -> Option<(String, &str)> {
    let input = input.strip_prefix('"')?;
    let end = input.find('"')?;
    let value = &input[..end];
    (!value.contains(['\\', '\n', '\r']) && value.chars().all(|ch| ch >= ' '))
        .then(|| (value.to_owned(), &input[end + 1..]))
}
fn wire_error(status: u16, code: &'static str) -> WireResponse {
    WireResponse {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: format!(r#"{{"code":"{code}","message":"request rejected"}}"#).into_bytes(),
    }
}
fn host_error(error: Error) -> WireResponse {
    let (status, code) = match error {
        Error::Limit => (413, "live.limit"),
        Error::Closed => (410, "live.expired"),
        Error::Denied => (403, "live.denied"),
        _ => (400, "live.malformed_request"),
    };
    wire_error(status, code)
}
fn session_response(
    metadata: &SessionMetadata,
    token: [u8; 32],
    limits: TransportLimits,
    status: u16,
) -> WireResponse {
    let path = format!("/orna/live/{}", uuid(metadata.session));
    let body = format!(
        "{{\"session\":\"{}\",\"database\":\"{}\",\"runtime\":\"{}\",\"resume_token\":\"{}\",\"websocket_path\":\"{}\",\"lease_ms\":{},\"limits\":{{\"max_message_bytes\":{},\"max_depth\":64,\"max_nodes\":100000,\"max_collection_items\":100000,\"max_outgoing_bytes\":{},\"request_retention_ms\":{}}}}}",
        uuid(metadata.session),
        uuid(metadata.database),
        uuid(metadata.runtime),
        encode_token(token),
        path,
        limits.lease_ms,
        limits.max_frame_bytes,
        limits.max_outgoing_bytes,
        limits.request_retention_ms
    );
    let mut cookie = format!(
        "orna_session={}; Path={path}; HttpOnly; SameSite=Strict",
        encode_token(token)
    );
    if limits.tls {
        cookie.push_str("; Secure");
    }
    WireResponse {
        status,
        headers: vec![
            ("content-type".into(), "application/json".into()),
            ("set-cookie".into(), cookie),
        ],
        body: body.into_bytes(),
    }
}
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
fn encode_token(token: [u8; 32]) -> String {
    let mut out = String::with_capacity(43);
    for chunk in token.chunks(3) {
        out.push(B64[(chunk[0] >> 2) as usize] as char);
        out.push(
            B64[(((chunk[0] & 3) << 4) | (chunk.get(1).copied().unwrap_or(0) >> 4)) as usize]
                as char,
        );
        if chunk.len() > 1 {
            out.push(
                B64[(((chunk[1] & 15) << 2) | (chunk.get(2).copied().unwrap_or(0) >> 6)) as usize]
                    as char,
            );
        }
        if chunk.len() > 2 {
            out.push(B64[(chunk[2] & 63) as usize] as char);
        }
    }
    out
}
fn decode_token(value: &str) -> Option<[u8; 32]> {
    (value.len() == 43)
        .then(|| decode_base64url(value))
        .flatten()
        .and_then(|bytes| bytes.try_into().ok())
}
fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let value = value.trim_end_matches('=');
    (!value.contains('='))
        .then(|| decode_base64url(value))
        .flatten()
}
#[allow(clippy::cast_possible_truncation)]
fn decode_base64url(value: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for byte in value.bytes() {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(digit);
        count += 6;
        while count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    (count == 0 || bits == 0).then_some(out)
}

#[derive(Debug)]
pub struct WebSocketState {
    attachment: [u8; 16],
    fragment: Option<(u8, Vec<u8>)>,
    pending: Vec<u8>,
    closed: bool,
}
impl WebSocketState {
    #[must_use]
    pub fn new(attachment: [u8; 16]) -> Self {
        Self {
            attachment,
            fragment: None,
            pending: Vec::new(),
            closed: false,
        }
    }
    fn push_one(&mut self, bytes: &[u8], limit: usize) -> Result<Option<SocketEvent>> {
        if self.closed {
            return Err(Error::Closed);
        }
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > limit.saturating_add(14) {
            return Err(Error::Limit);
        }
        let Some((used, fin, opcode, payload)) = ws_frame(&self.pending, limit)? else {
            return Ok(None);
        };
        self.pending.drain(..used);
        match opcode {
            0 => {
                let Some((kind, mut whole)) = self.fragment.take() else {
                    return Err(Error::InvalidFrame);
                };
                whole.extend(payload);
                if whole.len() > limit {
                    return Err(Error::Limit);
                }
                if fin {
                    Ok(Some(if kind == 2 {
                        SocketEvent::Binary(whole)
                    } else {
                        SocketEvent::Text
                    }))
                } else {
                    self.fragment = Some((kind, whole));
                    Ok(None)
                }
            }
            1 | 2 => {
                if self.fragment.is_some() {
                    return Err(Error::InvalidFrame);
                }
                if fin {
                    Ok(Some(if opcode == 2 {
                        SocketEvent::Binary(payload)
                    } else {
                        SocketEvent::Text
                    }))
                } else {
                    self.fragment = Some((opcode, payload));
                    Ok(None)
                }
            }
            8 => {
                validate_close_payload(&payload)?;
                self.closed = true;
                self.fragment = None;
                self.pending.clear();
                Ok(Some(SocketEvent::Close))
            }
            9 => Ok(Some(SocketEvent::Ping(payload))),
            10 => Ok(Some(SocketEvent::Pong)),
            _ => Err(Error::InvalidFrame),
        }
    }

    fn has_complete_frame(&self, limit: usize) -> Result<bool> {
        Ok(ws_frame(&self.pending, limit)?.is_some())
    }
}
enum SocketEvent {
    Binary(Vec<u8>),
    Text,
    Ping(Vec<u8>),
    Pong,
    Close,
}
type ParsedFrame = (usize, bool, u8, Vec<u8>);

fn validate_close_payload(payload: &[u8]) -> Result<()> {
    if payload.is_empty() {
        return Ok(());
    }
    if payload.len() == 1 {
        return Err(Error::InvalidFrame);
    }
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    if !(matches!(code, 1000..=1003 | 1007..=1014) || matches!(code, 3000..=4999)) {
        return Err(Error::InvalidFrame);
    }
    core::str::from_utf8(&payload[2..])
        .map(|_| ())
        .map_err(|_| Error::InvalidFrame)
}

fn ws_frame(bytes: &[u8], limit: usize) -> Result<Option<ParsedFrame>> {
    if bytes.len() < 2 {
        return Ok(None);
    }
    let first = bytes[0];
    let second = bytes[1];
    let fin = first & 128 != 0;
    let opcode = first & 15;
    if first & 0x70 != 0 || second & 0x80 == 0 {
        return Err(Error::InvalidFrame);
    }
    let mut at: usize = 2;
    let mut len = usize::from(second & 127);
    if len == 126 {
        if bytes.len() < 4 {
            return Ok(None);
        }
        len = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
        at = 4;
    } else if len == 127 {
        if bytes.len() < 10 {
            return Ok(None);
        }
        let size = u64::from_be_bytes(bytes[2..10].try_into().map_err(|_| Error::InvalidFrame)?);
        len = usize::try_from(size).map_err(|_| Error::Limit)?;
        at = 10;
    }
    if len > limit || matches!(opcode, 8..=10) && (!fin || len > 125) {
        return Err(Error::InvalidFrame);
    }
    let total = at.saturating_add(4).saturating_add(len);
    if bytes.len() < total {
        return Ok(None);
    }
    let key = &bytes[at..at + 4];
    let payload = bytes[at + 4..total]
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ key[index % 4])
        .collect();
    Ok(Some((total, fin, opcode, payload)))
}
fn websocket_accept(key: &str) -> String {
    base64_standard(&sha1(
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    ))
}
fn base64_standard(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for part in bytes.chunks(3) {
        out.push(TABLE[(part[0] >> 2) as usize] as char);
        out.push(
            TABLE[(((part[0] & 3) << 4) | (part.get(1).copied().unwrap_or(0) >> 4)) as usize]
                as char,
        );
        out.push(if part.len() > 1 {
            TABLE[(((part[1] & 15) << 2) | (part.get(2).copied().unwrap_or(0) >> 6)) as usize]
                as char
        } else {
            '='
        });
        out.push(if part.len() > 2 {
            TABLE[(part[2] & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
#[allow(clippy::many_single_char_names)]
fn sha1(input: &[u8]) -> [u8; 20] {
    let mut data = input.to_vec();
    let bits = (data.len() as u64) * 8;
    data.push(128);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_be_bytes());
    let (mut h0, mut h1, mut h2, mut h3, mut h4) = (
        0x6745_2301_u32,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    );
    for block in data.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in w[..16].iter_mut().enumerate() {
            *word = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }
    let mut out = [0; 20];
    for (i, value) in [h0, h1, h2, h3, h4].into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedIssuer(Option<[u8; 32]>);

    impl CredentialIssuer for FixedIssuer {
        fn issue_credential(&mut self) -> std::result::Result<[u8; 32], BoundaryError> {
            let credential = [7; 32];
            self.0 = Some(credential);
            Ok(credential)
        }
    }

    impl LiveCredentialIssuer for FixedIssuer {
        fn last_issued(&self) -> Option<[u8; 32]> {
            self.0
        }
    }

    fn result(request: u8, fingerprint: u8) -> Envelope {
        Envelope {
            request: Some([request; 16]),
            watch: None,
            message: Message::Result {
                status: ResultStatus::Failure,
                value: None,
                fingerprint: [fingerprint; 32],
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn control_responses_reject_non_unit_results() {
        assert_eq!(
            validate_control_response([1; 16], [2; 32], result(1, 2), ProtocolLimits::default(),),
            Err(Error::ApplicationRejected)
        );
    }

    #[test]
    fn result_response_rejects_a_different_request_fingerprint() {
        assert_eq!(
            validate_result_response([1; 16], [2; 32], result(1, 3), ProtocolLimits::default(),),
            Err(Error::RequestMismatch)
        );
    }

    #[test]
    fn request_status_replays_only_the_matching_retained_result_body() {
        let retained = retained_without_value_outcome([1; 16], [2; 32]);
        assert!(
            retained_result_body([1; 16], [2; 32], Some(&retained), ProtocolLimits::default(),)
                .is_some()
        );
        assert!(
            retained_result_body([1; 16], [3; 32], Some(&retained), ProtocolLimits::default(),)
                .is_none()
        );

        let response = retained.response.unwrap();
        let terminal =
            TerminalOutcome::new(response.encode(ProtocolLimits::default()).unwrap()).unwrap();
        let durable = DurableRequestStatus {
            identity: RequestIdentity {
                session_id: [4; 16],
                request_id: [1; 16],
            },
            fingerprint: [2; 32],
            state: DurableRequestState::Orphaned,
            terminal_outcome: Some(terminal),
        };
        assert!(
            durable_result_body([1; 16], [2; 32], &durable, ProtocolLimits::default(),).is_some()
        );
    }

    #[test]
    fn system_credential_issuer_retains_only_the_last_csprng_result() {
        let mut issuer = SystemCredentialIssuer::default();
        assert_eq!(issuer.last_issued(), None);
        let token = issuer.issue_credential().unwrap();
        assert_ne!(token, [0; 32]);
        assert_eq!(issuer.last_issued(), Some(token));
    }

    #[test]
    fn malformed_post_upgrade_frame_closes_its_attachment() {
        let origin = Origin::parse("https://app.example").unwrap();
        let mut host = LiveHost::new(
            Limits::default(),
            SessionBoundary::new(
                orna_security_v1::OriginPolicy::new([origin.clone()], []),
                10,
            ),
            Serving::new(orna_serving_v1::Limits::default()).unwrap(),
        )
        .unwrap();
        let subscribe = Envelope {
            request: Some([2; 16]),
            watch: None,
            message: Message::Subscribe {
                resource: [3; 16],
                presentation: orna_protocol_v1::PresentationContext {
                    locale: "en-GB".into(),
                    timezone: None,
                    width: None,
                    theme: "dark".into(),
                    supported_kinds: vec![],
                },
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap();
        let mut issuer = FixedIssuer(None);
        let credential = futures::executor::block_on(host.create(
            CreateRequest {
                id: [1; 16],
                origin: origin.clone(),
                expires_at: 10,
                now: 0,
                subscribe: &subscribe,
            },
            &mut issuer,
        ))
        .unwrap();
        let mut transport = LiveTransport::new(host, TransportLimits::default()).unwrap();
        transport.sessions.insert(
            [1; 16],
            SessionRecord {
                metadata: SessionMetadata {
                    session: [1; 16],
                    database: [2; 16],
                    runtime: [3; 16],
                    expires_at: 10,
                    subscribe,
                },
                credential,
            },
        );
        let mut input = format!(
            "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
            uuid([1; 16]),
            SUBPROTOCOL,
            encode_token([7; 32]),
        )
        .into_bytes();
        input.extend([0x89, 0x00]);
        let mut reader = futures::io::Cursor::new(input);
        let mut writer = futures::io::Cursor::new(Vec::new());
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut clock = || 1;
        let mut cancellation = std::future::pending::<()>();
        let mut application = RejectApplication;

        assert_eq!(
            futures::executor::block_on(transport.serve_websocket_connection(
                &mut reader,
                &mut writer,
                &mut connection,
                [4; 16],
                &mut clock,
                &mut cancellation,
                &mut application,
            )),
            Err(HttpIoError::Transport(HttpConnectionError::Protocol(
                Error::InvalidFrame
            )))
        );
        assert_eq!(
            futures::executor::block_on(transport.host.handle_frame([4; 16], 2, Frame::Close)),
            Err(Error::Closed)
        );
    }
}
