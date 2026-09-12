//! Bounded bootstrap for one protocol-level live subscription.
//!
//! This module starts after authentication and WebSocket negotiation.  The
//! transport supplied by the caller already implements the authenticated
//! binary-message boundary; this module does not implement TLS or RFC 6455.

use std::{future::Future, pin::Pin};

use orna_protocol_v1::{Envelope, Error as ProtocolError, Limits, Message};

use crate::live_session::{
    AuthenticatedLiveTransport, LiveSessionDriver, LiveSessionError, PresentRenderer,
    RequestIdAllocator,
};
use crate::live_transport::{LiveClient, LiveSession, LiveTransportError};
use tokio::net::TcpStream;
use tokio_tungstenite::MaybeTlsStream;

/// Errors while admitting the initial typed subscription and snapshot.
#[derive(Debug)]
pub enum LiveBootstrapError<E> {
    Io(E),
    Protocol(ProtocolError),
    InvalidRequest,
    UnexpectedResponse,
    WatchIdentity,
}

#[derive(Debug)]
pub enum LiveReconnectCause {
    Transport(LiveTransportError),
    Bootstrap(LiveBootstrapError<LiveTransportError>),
    LimitsChanged,
    WatchIdentity,
}

pub struct LiveReconnectFailure {
    session: LiveSession,
    cause: LiveReconnectCause,
}

impl LiveReconnectFailure {
    pub fn session(&self) -> &LiveSession {
        &self.session
    }

    pub fn into_session(self) -> LiveSession {
        self.session
    }

    pub fn cause(&self) -> &LiveReconnectCause {
        &self.cause
    }
}

pub enum LiveReconnectError {
    Resume(LiveTransportError),
    AfterResume(LiveReconnectFailure),
}

impl LiveReconnectError {
    pub fn rotated_session(&self) -> Option<&LiveSession> {
        match self {
            Self::Resume(_) => None,
            Self::AfterResume(failure) => Some(failure.session()),
        }
    }

    pub fn into_rotated_session(self) -> Option<LiveSession> {
        match self {
            Self::Resume(_) => None,
            Self::AfterResume(failure) => Some(failure.into_session()),
        }
    }
}

/// A binary transport which returns the bootstrap snapshot once, then
/// delegates to the same authenticated transport used during bootstrap.
pub struct PrefetchedBinaryTransport<I> {
    inner: I,
    first: Option<Vec<u8>>,
}

impl<I> PrefetchedBinaryTransport<I> {
    fn new(inner: I, first: Vec<u8>) -> Self {
        Self {
            inner,
            first: Some(first),
        }
    }
}

impl<I> crate::live_session::LiveByteDriver for PrefetchedBinaryTransport<I>
where
    I: crate::live_session::LiveByteDriver,
{
    type Error = I::Error;

    fn receive_binary<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Self::Error>> + 'a>> {
        if let Some(first) = self.first.take() {
            // Bootstrap checked this frame with the negotiated limit. The
            // existing driver performs its own defence-in-depth check if a
            // caller later supplies a smaller limit; never drop this frame.
            let _ = max_bytes;
            return Box::pin(async move { Ok(first) });
        }
        self.inner.receive_binary(max_bytes)
    }

    fn send_binary<'a>(
        &'a mut self,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + 'a>> {
        self.inner.send_binary(bytes)
    }
}

impl<I> AuthenticatedLiveTransport for PrefetchedBinaryTransport<I> where
    I: AuthenticatedLiveTransport
{
}

/// An authenticated subscription whose first snapshot is ready for the
/// existing [`LiveSessionDriver`] without being decoded or consumed twice.
pub struct BootstrappedLiveAttachment<I> {
    transport: PrefetchedBinaryTransport<I>,
    watch: [u8; 16],
    limits: Limits,
}

impl<I> BootstrappedLiveAttachment<I>
where
    I: AuthenticatedLiveTransport,
{
    /// Sends one existing-protocol `Subscribe` request and validates the
    /// correlated server snapshot.  Authentication and attachment admission
    /// are intentionally outside this API.
    pub async fn start(
        mut io: I,
        request: Envelope,
        limits: Limits,
    ) -> Result<Self, LiveBootstrapError<I::Error>> {
        if request.request.is_none()
            || request.watch.is_some()
            || !matches!(request.message, Message::Subscribe { .. })
        {
            return Err(LiveBootstrapError::InvalidRequest);
        }
        let encoded = request
            .encode(limits)
            .map_err(LiveBootstrapError::Protocol)?;
        io.send_binary(encoded)
            .await
            .map_err(LiveBootstrapError::Io)?;
        let first = io
            .receive_binary(limits.max_message_bytes)
            .await
            .map_err(LiveBootstrapError::Io)?;
        if first.len() > limits.max_message_bytes {
            return Err(LiveBootstrapError::Protocol(ProtocolError::Limit));
        }
        let response = Envelope::decode(&first, limits).map_err(LiveBootstrapError::Protocol)?;
        if response.request != request.request
            || response.watch.is_none()
            || !matches!(response.message, Message::Snapshot { .. })
        {
            return Err(LiveBootstrapError::UnexpectedResponse);
        }
        Ok(Self {
            transport: PrefetchedBinaryTransport::new(io, first),
            watch: response.watch.expect("checked above"),
            limits,
        })
    }

    /// Admits the complete automatic snapshot sent for a session-owned watch
    /// after session resumption. This path deliberately sends no client frame
    /// and accepts only a null request identity for the existing watch.
    pub async fn resume_existing_watch(
        mut io: I,
        watch: [u8; 16],
        limits: Limits,
    ) -> Result<Self, LiveBootstrapError<I::Error>> {
        let first = io
            .receive_binary(limits.max_message_bytes)
            .await
            .map_err(LiveBootstrapError::Io)?;
        if first.len() > limits.max_message_bytes {
            return Err(LiveBootstrapError::Protocol(ProtocolError::Limit));
        }
        let response = Envelope::decode(&first, limits).map_err(LiveBootstrapError::Protocol)?;
        if response.request.is_some()
            || response.watch != Some(watch)
            || !matches!(response.message, Message::Snapshot { .. })
        {
            return Err(LiveBootstrapError::UnexpectedResponse);
        }
        Ok(Self {
            transport: PrefetchedBinaryTransport::new(io, first),
            watch,
            limits,
        })
    }

    pub const fn watch(&self) -> [u8; 16] {
        self.watch
    }

    /// Transfers the authenticated binary transport and preserved first
    /// snapshot to the existing bounded presentation lifecycle owner.
    pub fn into_driver<R, A>(
        self,
        renderer: R,
        request_ids: A,
    ) -> Result<
        LiveSessionDriver<PrefetchedBinaryTransport<I>, R, A>,
        LiveSessionError<I::Error, R::Error>,
    >
    where
        R: PresentRenderer,
        A: RequestIdAllocator,
    {
        LiveSessionDriver::new(
            self.transport,
            self.watch,
            self.limits,
            renderer,
            request_ids,
        )
    }

    pub fn replace_driver<R, A>(
        self,
        driver: &mut LiveSessionDriver<PrefetchedBinaryTransport<I>, R, A>,
    ) -> Result<(), LiveBootstrapError<I::Error>>
    where
        R: PresentRenderer,
        A: RequestIdAllocator,
    {
        if self.watch == driver.watch() {
            return Err(LiveBootstrapError::WatchIdentity);
        }
        if self.limits != driver.limits() {
            return Err(LiveBootstrapError::UnexpectedResponse);
        }
        driver
            .replace_authenticated_attachment_with_watch(self.transport, self.watch)
            .map_err(|_| LiveBootstrapError::WatchIdentity)
    }

    pub fn replace_existing_watch<R, A>(
        self,
        driver: &mut LiveSessionDriver<PrefetchedBinaryTransport<I>, R, A>,
    ) -> Result<(), LiveBootstrapError<I::Error>>
    where
        R: PresentRenderer,
        A: RequestIdAllocator,
    {
        if self.watch != driver.watch() {
            return Err(LiveBootstrapError::WatchIdentity);
        }
        if self.limits != driver.limits() {
            return Err(LiveBootstrapError::UnexpectedResponse);
        }
        driver.replace_authenticated_attachment(self.transport);
        Ok(())
    }
}

impl LiveClient {
    /// Resumes the HTTP session, opens a new authenticated WebSocket, and
    /// resubscribes with `request`, receiving a complete snapshot for a new
    /// watch before replacing the driver.
    pub async fn reconnect_driver<R, A>(
        &self,
        session: &LiveSession,
        driver: &mut LiveSessionDriver<
            PrefetchedBinaryTransport<
                crate::live_transport::AuthenticatedWebSocketTransport<MaybeTlsStream<TcpStream>>,
            >,
            R,
            A,
        >,
        request: Envelope,
    ) -> Result<LiveSession, LiveReconnectError>
    where
        R: PresentRenderer,
        A: RequestIdAllocator,
    {
        let mut replacement = Some(
            self.resume_session(session)
                .await
                .map_err(LiveReconnectError::Resume)?,
        );
        if replacement.as_ref().unwrap().runtime_id() != session.runtime_id() {
            return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                session: replacement.take().unwrap(),
                cause: LiveReconnectCause::Transport(LiveTransportError::Response(
                    "resume response changed runtime identity",
                )),
            }));
        }
        if replacement.as_ref().unwrap().limits() != driver.limits() {
            return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                session: replacement.take().unwrap(),
                cause: LiveReconnectCause::LimitsChanged,
            }));
        }
        let replacement_limits = replacement.as_ref().unwrap().limits();
        let transport = match self.connect(replacement.as_ref().unwrap()).await {
            Ok(transport) => transport,
            Err(error) => {
                return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                    session: replacement.take().unwrap(),
                    cause: LiveReconnectCause::Transport(error),
                }));
            }
        };
        let attachment =
            match BootstrappedLiveAttachment::start(transport, request, replacement_limits).await {
                Ok(attachment) => attachment,
                Err(error) => {
                    return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                        session: replacement.take().unwrap(),
                        cause: LiveReconnectCause::Bootstrap(error),
                    }));
                }
            };
        if let Err(error) = attachment.replace_driver(driver) {
            let cause = match error {
                LiveBootstrapError::WatchIdentity => LiveReconnectCause::WatchIdentity,
                other => LiveReconnectCause::Bootstrap(other),
            };
            return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                session: replacement.take().unwrap(),
                cause,
            }));
        }
        Ok(replacement.take().unwrap())
    }

    /// Resumes the HTTP session and reattaches the existing session-owned
    /// watch. The host sends a null-request automatic snapshot; no client
    /// request is written to the replacement WebSocket.
    pub async fn resume_driver<R, A>(
        &self,
        session: &LiveSession,
        driver: &mut LiveSessionDriver<
            PrefetchedBinaryTransport<
                crate::live_transport::AuthenticatedWebSocketTransport<MaybeTlsStream<TcpStream>>,
            >,
            R,
            A,
        >,
    ) -> Result<LiveSession, LiveReconnectError>
    where
        R: PresentRenderer,
        A: RequestIdAllocator,
    {
        let mut replacement = Some(
            self.resume_session(session)
                .await
                .map_err(LiveReconnectError::Resume)?,
        );
        if replacement.as_ref().unwrap().runtime_id() != session.runtime_id() {
            return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                session: replacement.take().unwrap(),
                cause: LiveReconnectCause::Transport(LiveTransportError::Response(
                    "resume response changed runtime identity",
                )),
            }));
        }
        if replacement.as_ref().unwrap().limits() != driver.limits() {
            return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                session: replacement.take().unwrap(),
                cause: LiveReconnectCause::LimitsChanged,
            }));
        }
        let replacement_limits = replacement.as_ref().unwrap().limits();
        let transport = match self.connect(replacement.as_ref().unwrap()).await {
            Ok(transport) => transport,
            Err(error) => {
                return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                    session: replacement.take().unwrap(),
                    cause: LiveReconnectCause::Transport(error),
                }));
            }
        };
        let attachment = match BootstrappedLiveAttachment::resume_existing_watch(
            transport,
            driver.watch(),
            replacement_limits,
        )
        .await
        {
            Ok(attachment) => attachment,
            Err(error) => {
                return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                    session: replacement.take().unwrap(),
                    cause: LiveReconnectCause::Bootstrap(error),
                }));
            }
        };
        if let Err(error) = attachment.replace_existing_watch(driver) {
            let cause = match error {
                LiveBootstrapError::WatchIdentity => LiveReconnectCause::WatchIdentity,
                other => LiveReconnectCause::Bootstrap(other),
            };
            return Err(LiveReconnectError::AfterResume(LiveReconnectFailure {
                session: replacement.take().unwrap(),
                cause,
            }));
        }
        Ok(replacement.take().unwrap())
    }
}
