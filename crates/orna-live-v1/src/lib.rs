//! A small transport-facing adapter for canonical `orna.present.v1` sessions.
//!
//! Hosts provide request routing, credential issuance, and durable deletion;
//! this crate owns no socket or HTTP implementation. All public failures are
//! stable redacted codes and inbound protocol bytes are decoded canonically.

use std::collections::BTreeMap;

use orna_protocol_v1::{Envelope, Limits as ProtocolLimits, Message, RequestState};
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
    fingerprints: BTreeMap<([u8; 16], [u8; 16]), [u8; 32]>,
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
            fingerprints: BTreeMap::new(),
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
        let mut application = RejectApplication;
        Ok(self
            .dispatch_frame(attachment, now, frame, &mut application)
            .await?
            .outcome)
    }

    /// Dispatches one complete client frame through the full client registry.
    /// Eval and watch require the supplied application seam; the compatibility
    /// method above explicitly rejects them.
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
                match &envelope.message {
                    Message::Cancel { .. } => {
                        self.serving
                            .cancel_envelope(session, &envelope)
                            .map_err(map_serving)?;
                        Ok(DispatchOutcome {
                            outcome: FrameOutcome::Cancelled,
                            response: None,
                        })
                    }
                    Message::Resync => {
                        let revisions = self.serving.resync(session, 0).map_err(map_serving)?.len();
                        Ok(DispatchOutcome {
                            outcome: FrameOutcome::Resync { revisions },
                            response: None,
                        })
                    }
                    Message::RequestStatus {
                        target,
                        fingerprint,
                    } => {
                        let retained = self.fingerprints.get(&(session, *target)).copied();
                        if retained.is_some_and(|known| known != *fingerprint) {
                            return Err(Error::RequestMismatch);
                        }
                        let state = match self.serving.request_state(session, *target) {
                            Ok(orna_serving_v1::RequestState::Reserved) => RequestState::Reserved,
                            Ok(orna_serving_v1::RequestState::Running) => RequestState::Running,
                            Ok(
                                orna_serving_v1::RequestState::Cancelled
                                | orna_serving_v1::RequestState::Completed,
                            ) => RequestState::Terminal,
                            Err(ServingError::RequestUnknown) => RequestState::Unknown,
                            Err(error) => return Err(map_serving(error)),
                        };
                        Ok(DispatchOutcome {
                            outcome: FrameOutcome::Accepted,
                            response: Some(Envelope {
                                request: Some(request),
                                watch: None,
                                message: Message::RequestStatusResult {
                                    target: *target,
                                    state,
                                    fingerprint: retained,
                                    result: None,
                                },
                                extensions: BTreeMap::new(),
                            }),
                        })
                    }
                    Message::Eval { fingerprint, .. } => {
                        match self.fingerprints.get(&(session, request)) {
                            Some(known) if *known != *fingerprint => {
                                return Err(Error::RequestMismatch);
                            }
                            Some(_) => {}
                            None => {
                                self.fingerprints.insert((session, request), *fingerprint);
                            }
                        }
                        let response = application.eval(session, request, &envelope.message)?;
                        validate_eval_response(request, *fingerprint, response)
                    }
                    Message::Watch { .. } => {
                        let response = application.watch(session, request, &envelope.message)?;
                        validate_watch_response(request, response)
                    }
                    Message::Subscribe { .. }
                    | Message::Unsubscribe
                    | Message::Event { .. }
                    | Message::Snapshot { .. }
                    | Message::Delta { .. }
                    | Message::Result { .. }
                    | Message::Diagnostic { .. }
                    | Message::RequestStatusResult { .. } => Err(Error::UnsupportedOperation),
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

fn validate_eval_response(
    request: [u8; 16],
    fingerprint: [u8; 32],
    response: Envelope,
) -> Result<DispatchOutcome> {
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

fn validate_watch_response(request: [u8; 16], response: Envelope) -> Result<DispatchOutcome> {
    if response.request != Some(request) || response.watch.is_none() {
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
pub enum WebSocketOutput {
    Accepted(FrameOutcome),
    Pong,
    Close,
}

#[derive(Clone)]
struct SessionRecord {
    metadata: SessionMetadata,
    credential: SessionCredential,
}

/// A bounded HTTP/RFC-6455 parser and adapter. It deliberately has no listener,
/// socket, clock, TLS or authentication implementation; those stay at the
/// executable host edge.
pub struct LiveTransport {
    host: LiveHost,
    limits: TransportLimits,
    sessions: BTreeMap<[u8; 16], SessionRecord>,
}

impl LiveTransport {
    /// # Errors
    ///
    /// Returns [`Error::Limit`] for an invalid transport bound.
    pub fn new(host: LiveHost, limits: TransportLimits) -> Result<Self> {
        Ok(Self {
            limits: limits.validate(host.limits.protocol)?,
            host,
            sessions: BTreeMap::new(),
        })
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
                match self.host.rotate(id, &origin, &supplied, now, issuer).await {
                    Ok(credential) => {
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

    /// Validates the RFC 6455 handshake and attaches the cookie-authenticated
    /// session. The caller supplies its socket identity after accepting bytes.
    pub async fn upgrade(
        &mut self,
        request: WireRequest,
        attachment: [u8; 16],
        now: u64,
    ) -> WireResponse {
        if !request.body.is_empty() || header_size(&request.headers) > self.limits.max_header_bytes
        {
            return wire_error(400, "live.malformed_request");
        }
        let SessionPath::Live(id) = session_path(&request.path) else {
            return wire_error(400, "live.malformed_request");
        };
        let Ok(origin) = required_origin(&request.headers) else {
            return wire_error(403, "live.origin_denied");
        };
        let Some(record) = self.sessions.get(&id) else {
            return wire_error(410, "live.expired");
        };
        let Some(cookie) =
            cookie(&request.headers, "orna_session").and_then(|value| decode_token(&value))
        else {
            return wire_error(401, "live.unauthenticated");
        };
        if request.method != "GET"
            || !header_token(&request.headers, "connection", "upgrade")
            || !header_eq(&request.headers, "upgrade", "websocket")
            || !header_eq(&request.headers, "sec-websocket-version", "13")
            || !header_token(&request.headers, "sec-websocket-protocol", SUBPROTOCOL)
        {
            return wire_error(400, "live.malformed_request");
        }
        let Some(key) = header(&request.headers, "sec-websocket-key").and_then(decode_base64)
        else {
            return wire_error(400, "live.malformed_request");
        };
        if key.len() != 16 {
            return wire_error(400, "live.malformed_request");
        }
        let credential = SessionCredential {
            security: OpaqueCredential::from_bytes(cookie),
            serving: ServingCredential::new(cookie),
        };
        if credential != record.credential {
            return wire_error(401, "live.unauthenticated");
        }
        match self
            .host
            .resume(ResumeRequest {
                id,
                origin: &origin,
                credential: &credential,
                attachment,
                now,
            })
            .await
        {
            Ok(_) => WireResponse {
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
            Err(Error::Closed | Error::Denied) => wire_error(410, "live.expired"),
            Err(error) => host_error(error),
        }
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
        let events = socket.push(bytes, self.limits.max_frame_bytes)?;
        let mut output = Vec::new();
        for event in events {
            match event {
                SocketEvent::Binary(message) => output.push(WebSocketOutput::Accepted(
                    self.host
                        .handle_frame(socket.attachment, now, Frame::Binary(message))
                        .await?,
                )),
                SocketEvent::Text => return Err(Error::InvalidFrame),
                SocketEvent::Ping => output.push(WebSocketOutput::Pong),
                SocketEvent::Pong => {}
                SocketEvent::Close => {
                    output.push(WebSocketOutput::Accepted(
                        self.host
                            .handle_frame(socket.attachment, now, Frame::Close)
                            .await?,
                    ));
                    output.push(WebSocketOutput::Close);
                }
            }
        }
        Ok(output)
    }
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
}
impl WebSocketState {
    #[must_use]
    pub fn new(attachment: [u8; 16]) -> Self {
        Self {
            attachment,
            fragment: None,
            pending: Vec::new(),
        }
    }
    fn push(&mut self, bytes: &[u8], limit: usize) -> Result<Vec<SocketEvent>> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > limit.saturating_add(14) {
            return Err(Error::Limit);
        }
        let mut events = Vec::new();
        while let Some((used, fin, opcode, payload)) = ws_frame(&self.pending, limit)? {
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
                        events.push(if kind == 2 {
                            SocketEvent::Binary(whole)
                        } else {
                            SocketEvent::Text
                        });
                    } else {
                        self.fragment = Some((kind, whole));
                    }
                }
                1 | 2 => {
                    if self.fragment.is_some() {
                        return Err(Error::InvalidFrame);
                    }
                    if fin {
                        events.push(if opcode == 2 {
                            SocketEvent::Binary(payload)
                        } else {
                            SocketEvent::Text
                        });
                    } else {
                        self.fragment = Some((opcode, payload));
                    }
                }
                8 => events.push(SocketEvent::Close),
                9 => events.push(SocketEvent::Ping),
                10 => events.push(SocketEvent::Pong),
                _ => return Err(Error::InvalidFrame),
            }
        }
        Ok(events)
    }
}
enum SocketEvent {
    Binary(Vec<u8>),
    Text,
    Ping,
    Pong,
    Close,
}
type ParsedFrame = (usize, bool, u8, Vec<u8>);

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
