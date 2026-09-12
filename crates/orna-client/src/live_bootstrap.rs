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

/// Errors while admitting the initial typed subscription and snapshot.
#[derive(Debug)]
pub enum LiveBootstrapError<E> {
    Io(E),
    Protocol(ProtocolError),
    InvalidRequest,
    UnexpectedResponse,
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
}
