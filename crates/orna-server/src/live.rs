//! One-shot loopback live-session host.
//!
//! This is the first executable-owned live boundary. It accepts one local
//! HTTP session-create connection and then exits. TLS, remote exposure,
//! WebSocket handoff, durable live-session rows, and application dispatch are
//! deliberately outside this slice.

use futures::{Future, executor::block_on};
use orna_live_v1::{
    HttpConnection, HttpIoError, Limits, LiveHost, LiveSessionAuthority, LiveTransport,
    SessionMetadata, SystemCredentialIssuer, TransportLimits,
};
use orna_protocol_v1::{Envelope, Limits as ProtocolLimits, Message, PresentationContext};
use orna_repository_v1::{Repository, inspect_metadata};
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_security_v1::{Origin, OriginPolicy, SessionBoundary, SessionDeletionAdapter, SessionId};
use orna_serving_v1::{Limits as ServingLimits, Serving};
use std::{
    cell::RefCell,
    collections::BTreeSet,
    fmt, io,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
    time::{SystemTime, UNIX_EPOCH},
};

/// Stable failures from the executable-owned live boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveHostError {
    Repository,
    Runtime,
    Configuration,
    Listener,
    Connection,
    Cancelled,
}

impl fmt::Display for LiveHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Repository => "orna: live repository authority unavailable",
            Self::Runtime => "orna: live runtime authority unavailable",
            Self::Configuration => "orna: live host configuration unavailable",
            Self::Listener => "orna: live loopback listener unavailable",
            Self::Connection => "orna: live HTTP connection failed",
            Self::Cancelled => "orna: live host cancelled",
        })
    }
}

impl std::error::Error for LiveHostError {}

/// One loopback-only live host. The listener and session state are owned by
/// this value until [`Self::serve`] completes one HTTP connection.
pub struct LiveOnceHost {
    listener: orna_live_v1::LiveListener,
    transport: LiveTransport,
    authority: HostAuthority,
    deletion: HostDeletion,
}

impl LiveOnceHost {
    /// Binds a one-shot host to the default loopback listener.
    pub fn bind(repository: &Repository, port: u16) -> Result<Self, LiveHostError> {
        let metadata = inspect_metadata(repository)
            .map_err(|_| LiveHostError::Repository)?
            .ok_or(LiveHostError::Repository)?;
        let database_id = *metadata.database_id().as_bytes();
        let (identity, initial_digest) = runtime_identity(database_id);
        let state = block_on(RuntimeState::open(repository, identity, initial_digest))
            .map_err(|_| LiveHostError::Runtime)?;
        let persisted = block_on(state.identity()).map_err(|_| LiveHostError::Runtime)?;
        if persisted.database_id != database_id || persisted != identity {
            return Err(LiveHostError::Runtime);
        }

        let origin = Origin::parse("http://localhost").map_err(|_| LiveHostError::Configuration)?;
        let host = LiveHost::new(
            Limits::default(),
            SessionBoundary::new(OriginPolicy::new([origin], []), 30_000),
            Serving::new(ServingLimits::default()).map_err(|_| LiveHostError::Configuration)?,
        )
        .map_err(|_| LiveHostError::Configuration)?;
        let transport = LiveTransport::new(host, TransportLimits::default())
            .map_err(|_| LiveHostError::Configuration)?;
        let listener =
            LiveTransport::bind_default_listener(port).map_err(|_| LiveHostError::Listener)?;
        let sessions = Rc::new(RefCell::new(BTreeSet::new()));
        Ok(Self {
            listener,
            transport,
            authority: HostAuthority {
                database_id,
                runtime_id: identity.repository_id,
                sessions: Rc::clone(&sessions),
            },
            deletion: HostDeletion { sessions },
        })
    }

    /// Returns the bound loopback address before the host accepts its peer.
    #[must_use]
    pub const fn address(&self) -> std::net::SocketAddr {
        self.listener.status().address
    }

    /// Accepts and serves exactly one bounded HTTP session-create connection.
    pub fn serve(mut self) -> Result<(), LiveHostError> {
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut issuer = SystemCredentialIssuer::default();
        let mut clock = system_seconds;
        self.transport
            .serve_one_http_listener(
                self.listener.listener(),
                &mut connection,
                &mut clock,
                &mut self.authority,
                &mut issuer,
                &mut self.deletion,
            )
            .map_err(|_| LiveHostError::Connection)
    }

    /// Serves one loopback connection while racing accept and connection I/O
    /// against the caller-owned cancellation future. The host owns the
    /// listener and accepted socket for the complete task lifetime.
    pub fn serve_with_cancellation<C>(mut self, mut cancellation: C) -> Result<(), LiveHostError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .map_err(|_| LiveHostError::Configuration)?;
        runtime.block_on(self.serve_with_cancellation_async(&mut cancellation))
    }

    /// Accepts sequential loopback connections until the caller cancels.
    /// Transport/session state remains owned by this host across connections.
    pub fn serve_until_cancellation<C>(mut self, mut cancellation: C) -> Result<(), LiveHostError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .map_err(|_| LiveHostError::Configuration)?;
        runtime.block_on(async {
            loop {
                match self.serve_with_cancellation_async(&mut cancellation).await {
                    Ok(()) | Err(LiveHostError::Connection) => {}
                    Err(error) => return Err(error),
                }
            }
        })
    }

    async fn serve_with_cancellation_async<C>(
        &mut self,
        cancellation: &mut C,
    ) -> Result<(), LiveHostError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let listener = self
            .listener
            .listener()
            .try_clone()
            .map_err(|_| LiveHostError::Listener)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| LiveHostError::Listener)?;
        let listener =
            tokio::net::TcpListener::from_std(listener).map_err(|_| LiveHostError::Listener)?;
        let (stream, _) = tokio::select! {
            result = listener.accept() => result.map_err(|_| LiveHostError::Connection)?,
            () = &mut *cancellation => return Err(LiveHostError::Cancelled),
        };
        let (reader, writer) = stream.into_split();
        let mut reader = TokioReader(reader);
        let mut writer = TokioWriter(writer);
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut issuer = SystemCredentialIssuer::default();
        let mut clock = system_seconds;
        self.transport
            .serve_http_connection_with_cancellation(
                &mut reader,
                &mut writer,
                &mut connection,
                &mut clock,
                cancellation,
                &mut self.authority,
                &mut issuer,
                &mut self.deletion,
            )
            .await
            .map_err(|error| match error {
                HttpIoError::Cancelled => LiveHostError::Cancelled,
                _ => LiveHostError::Connection,
            })
    }
}

struct TokioReader(tokio::net::tcp::OwnedReadHalf);

impl futures::io::AsyncRead for TokioReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut read_buffer = tokio::io::ReadBuf::new(buffer);
        tokio::io::AsyncRead::poll_read(Pin::new(&mut self.0), context, &mut read_buffer)
            .map(|result| result.map(|()| read_buffer.filled().len()))
    }
}

struct TokioWriter(tokio::net::tcp::OwnedWriteHalf);

impl futures::io::AsyncWrite for TokioWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write(Pin::new(&mut self.0), context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(Pin::new(&mut self.0), context)
    }

    fn poll_close(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(Pin::new(&mut self.0), context)
    }
}

struct HostAuthority {
    database_id: [u8; 16],
    runtime_id: [u8; 16],
    sessions: Rc<RefCell<BTreeSet<SessionId>>>,
}

impl LiveSessionAuthority for HostAuthority {
    fn create_session(
        &mut self,
        database: [u8; 16],
        now: u64,
    ) -> orna_live_v1::Result<SessionMetadata> {
        if database != self.database_id {
            return Err(orna_live_v1::Error::Denied);
        }
        let mut id = [0; 16];
        for _ in 0..8 {
            getrandom::fill(&mut id).map_err(|_| orna_live_v1::Error::Denied)?;
            if id != [0; 16] {
                let session = SessionId::new(id);
                if self.sessions.borrow_mut().insert(session) {
                    return Ok(SessionMetadata {
                        session: id,
                        database,
                        runtime: self.runtime_id,
                        expires_at: now.saturating_add(30_000),
                        subscribe: subscribe_payload(),
                    });
                }
            }
        }
        Err(orna_live_v1::Error::Denied)
    }
}

struct HostDeletion {
    sessions: Rc<RefCell<BTreeSet<SessionId>>>,
}

impl SessionDeletionAdapter for HostDeletion {
    type Error = ();

    fn delete(&mut self, session: SessionId) -> Result<(), Self::Error> {
        self.sessions.borrow_mut().remove(&session);
        Ok(())
    }
}

fn runtime_identity(database_id: [u8; 16]) -> (RuntimeIdentity, [u8; 32]) {
    let mut repository_id = database_id;
    for (index, byte) in repository_id.iter_mut().enumerate() {
        let rotation = u32::try_from(index % 7 + 1).expect("bounded rotation");
        let salt = u8::try_from(index).expect("fixed identity length");
        *byte = byte.rotate_left(rotation) ^ (0x5a_u8.wrapping_add(salt));
    }
    if repository_id == [0; 16] {
        repository_id[0] = 1;
    }
    let mut initial_digest = [0; 32];
    initial_digest[..16].copy_from_slice(&database_id);
    initial_digest[16..].copy_from_slice(&repository_id);
    (
        RuntimeIdentity {
            database_id,
            repository_id,
        },
        initial_digest,
    )
}

fn subscribe_payload() -> Vec<u8> {
    Envelope {
        request: Some([1; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [2; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/dark".into(),
                supported_kinds: vec![],
            },
        },
        extensions: std::collections::BTreeMap::new(),
    }
    .encode(ProtocolLimits::default())
    .expect("static subscribe payload")
}

fn system_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
