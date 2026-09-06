//! Loopback live-session host.
//!
//! This is the first executable-owned live boundary. Its basic entry point
//! accepts one local HTTP connection; the cancellable entry point retains
//! transport/session state across sequential connections and hands the live
//! WebSocket path to the bounded transport driver. TLS, remote exposure,
//! durable session credentials, and application dispatch are deliberately
//! outside this slice. Verified runtime request reservations and terminal
//! outcomes are retained through the existing runtime state boundary.

use futures::{
    Future, FutureExt,
    executor::block_on,
    io::{AsyncReadExt, AsyncWriteExt},
};
use orna_live_v1::{
    HttpConnection, HttpConnectionError, HttpIoError, Limits, LiveApplication, LiveHost,
    LiveSessionAuthority, LiveTransport, SessionMetadata, SystemCredentialIssuer, TransportLimits,
    WebSocketOutput, WebSocketState, encode_websocket_output, parse_http_request,
};
use orna_protocol_v1::{Envelope, Limits as ProtocolLimits, Message, PresentationContext};
use orna_repository_v1::{Repository, inspect_metadata};
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_security_v1::{Origin, OriginPolicy, SessionBoundary, SessionDeletionAdapter, SessionId};
use orna_serving_v1::{Limits as ServingLimits, Serving};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
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

        let listener =
            LiveTransport::bind_default_listener(port).map_err(|_| LiveHostError::Listener)?;
        let address = listener.status().address;
        let localhost = Origin::parse(format!("http://localhost:{}", address.port()))
            .map_err(|_| LiveHostError::Configuration)?;
        let loopback = Origin::parse(format!("http://127.0.0.1:{}", address.port()))
            .map_err(|_| LiveHostError::Configuration)?;
        let bare_localhost =
            Origin::parse("http://localhost").map_err(|_| LiveHostError::Configuration)?;
        let host = LiveHost::with_runtime_state(
            Limits::default(),
            SessionBoundary::new(
                OriginPolicy::new([bare_localhost, localhost, loopback], []),
                30_000,
            ),
            Serving::new(ServingLimits::default()).map_err(|_| LiveHostError::Configuration)?,
            state,
        )
        .map_err(|_| LiveHostError::Configuration)?;
        let transport = LiveTransport::new(host, TransportLimits::default())
            .map_err(|_| LiveHostError::Configuration)?;
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
        let mut clock = system_milliseconds;
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
    pub fn serve_until_cancellation<C>(self, mut cancellation: C) -> Result<(), LiveHostError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .map_err(|_| LiveHostError::Configuration)?;
        let local = tokio::task::LocalSet::new();
        local.block_on(
            &runtime,
            self.serve_concurrently_with_cancellation(&mut cancellation),
        )
    }

    async fn serve_concurrently_with_cancellation<C>(
        self,
        cancellation: &mut C,
    ) -> Result<(), LiveHostError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let LiveOnceHost {
            listener,
            transport,
            authority,
            deletion,
        } = self;
        let listener = listener
            .listener()
            .try_clone()
            .map_err(|_| LiveHostError::Listener)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| LiveHostError::Listener)?;
        let listener =
            tokio::net::TcpListener::from_std(listener).map_err(|_| LiveHostError::Listener)?;
        let (actor_sender, actor_receiver) = futures::channel::mpsc::unbounded();
        let actor = tokio::task::spawn_local(run_host_actor(
            actor_receiver,
            ConcurrentHostState {
                transport,
                authority,
                issuer: SystemCredentialIssuer::default(),
                deletion,
            },
        ));
        let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
        let mut workers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, _) = result.map_err(|_| LiveHostError::Connection)?;
                    let (worker_id, cancellation_receiver, done_sender) = registry.borrow_mut().register();
                    let actor = actor_sender.clone();
                    let registry = Rc::clone(&registry);
                    workers.spawn_local(async move {
                        serve_socket_worker(
                            stream,
                            actor,
                            Rc::clone(&registry),
                            worker_id,
                            cancellation_receiver,
                        )
                        .await;
                        registry.borrow_mut().remove(worker_id);
                        let _ = done_sender.send(());
                    });
                }
                result = workers.join_next(), if !workers.is_empty() => {
                    if result.is_some_and(|result| result.is_err()) {
                        continue;
                    }
                }
                () = &mut *cancellation => {
                    registry.borrow_mut().cancel_all();
                    while workers.join_next().await.is_some() {}
                    drop(actor_sender);
                    let _ = actor.await;
                    return Err(LiveHostError::Cancelled);
                }
            }
        }
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
        let reader = TokioReader(reader);
        let mut reader = PrefixedReader::new(reader);
        let mut writer = TokioWriter(writer);
        let initial = read_initial_request(&mut reader, cancellation).await?;
        let websocket = parse_http_request(&initial, TransportLimits::default())
            .map_err(|_| LiveHostError::Connection)?
            .is_some_and(|request| {
                request.request().method == "GET"
                    && request.request().path.starts_with("/orna/live/")
            });
        reader.replay(initial);
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut issuer = SystemCredentialIssuer::default();
        let mut clock = system_milliseconds;
        if websocket {
            let mut application = RejectLiveApplication;
            let attachment = opaque_attachment().map_err(|_| LiveHostError::Configuration)?;
            self.transport
                .serve_websocket_connection(
                    &mut reader,
                    &mut writer,
                    &mut connection,
                    attachment,
                    &mut clock,
                    cancellation,
                    &mut application,
                )
                .await
                .map_err(map_connection_error)
        } else {
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
                .map_err(map_connection_error)
        }
    }
}

struct ConcurrentHostState {
    transport: LiveTransport,
    authority: HostAuthority,
    issuer: SystemCredentialIssuer,
    deletion: HostDeletion,
}

enum ActorCommand {
    Http {
        connection: HttpConnection,
        bytes: Vec<u8>,
        reply: futures::channel::oneshot::Sender<Result<ActorHttpResult, HttpConnectionError>>,
    },
    Prepare {
        request: orna_live_v1::WireRequest,
        attachment: [u8; 16],
        now: u64,
        reply: futures::channel::oneshot::Sender<
            Result<orna_live_v1::WebSocketUpgrade, orna_live_v1::WireResponse>,
        >,
    },
    Commit {
        upgrade: orna_live_v1::WebSocketUpgrade,
        reply: futures::channel::oneshot::Sender<
            Result<(orna_live_v1::WireResponse, Vec<[u8; 16]>), orna_live_v1::Error>,
        >,
    },
    Receive {
        socket: WebSocketState,
        bytes: Vec<u8>,
        now: u64,
        reply: futures::channel::oneshot::Sender<
            Result<(WebSocketState, Vec<WebSocketOutput>), orna_live_v1::Error>,
        >,
    },
    Close {
        attachment: [u8; 16],
        now: u64,
        reply: futures::channel::oneshot::Sender<
            Result<orna_live_v1::FrameOutcome, orna_live_v1::Error>,
        >,
    },
}

struct ActorHttpResult {
    connection: HttpConnection,
    responses: Vec<Vec<u8>>,
    retired: Vec<[u8; 16]>,
}

async fn run_host_actor(
    mut commands: futures::channel::mpsc::UnboundedReceiver<ActorCommand>,
    mut state: ConcurrentHostState,
) {
    use futures::StreamExt;
    while let Some(command) = commands.next().await {
        match command {
            ActorCommand::Http {
                mut connection,
                bytes,
                reply,
            } => {
                let host = &mut state;
                let result = host
                    .transport
                    .handle_http_read(
                        &mut connection,
                        &bytes,
                        system_milliseconds(),
                        &mut host.authority,
                        &mut host.issuer,
                        &mut host.deletion,
                    )
                    .await
                    .map(|responses| ActorHttpResult {
                        connection,
                        responses,
                        retired: host.transport.take_retired_attachments(),
                    });
                let _ = reply.send(result);
            }
            ActorCommand::Prepare {
                request,
                attachment,
                now,
                reply,
            } => {
                let _ = reply.send(
                    state
                        .transport
                        .prepare_websocket_upgrade(&request, attachment, now),
                );
            }
            ActorCommand::Commit { upgrade, reply } => {
                let result = state
                    .transport
                    .commit_websocket_upgrade(upgrade)
                    .await
                    .map(|response| (response, state.transport.take_retired_attachments()));
                let _ = reply.send(result);
            }
            ActorCommand::Receive {
                mut socket,
                bytes,
                now,
                reply,
            } => {
                let result = state
                    .transport
                    .receive(&mut socket, now, &bytes)
                    .await
                    .map(|outputs| (socket, outputs));
                let _ = reply.send(result);
            }
            ActorCommand::Close {
                attachment,
                now,
                reply,
            } => {
                let _ = reply.send(state.transport.close_attachment(attachment, now).await);
            }
        }
    }
}

struct WorkerSlot {
    cancellation: futures::channel::oneshot::Sender<()>,
    done: futures::channel::oneshot::Receiver<()>,
}

#[derive(Default)]
struct WorkerRegistry {
    next_id: u64,
    workers: BTreeMap<u64, WorkerSlot>,
    attachments: BTreeMap<[u8; 16], u64>,
}

impl WorkerRegistry {
    fn register(
        &mut self,
    ) -> (
        u64,
        futures::channel::oneshot::Receiver<()>,
        futures::channel::oneshot::Sender<()>,
    ) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let (cancellation, cancellation_receiver) = futures::channel::oneshot::channel();
        let (done_sender, done) = futures::channel::oneshot::channel();
        self.workers.insert(id, WorkerSlot { cancellation, done });
        (id, cancellation_receiver, done_sender)
    }

    fn remove(&mut self, id: u64) {
        self.workers.remove(&id);
        self.attachments.retain(|_, worker| *worker != id);
    }

    fn attach(&mut self, attachment: [u8; 16], id: u64) {
        self.attachments.insert(attachment, id);
    }

    fn detach(&mut self, attachment: [u8; 16], id: u64) {
        if self.attachments.get(&attachment) == Some(&id) {
            self.attachments.remove(&attachment);
        }
    }

    fn take_retired(
        &mut self,
        attachments: Vec<[u8; 16]>,
    ) -> Vec<futures::channel::oneshot::Receiver<()>> {
        attachments
            .into_iter()
            .filter_map(|attachment| {
                let id = self.attachments.remove(&attachment)?;
                let slot = self.workers.remove(&id)?;
                let _ = slot.cancellation.send(());
                Some(slot.done)
            })
            .collect()
    }

    fn cancel_all(&mut self) {
        let workers = std::mem::take(&mut self.workers);
        for slot in workers.into_values() {
            let _ = slot.cancellation.send(());
        }
    }
}

async fn retire_and_join(registry: &Rc<RefCell<WorkerRegistry>>, attachments: Vec<[u8; 16]>) {
    let done = registry.borrow_mut().take_retired(attachments);
    for receiver in done {
        let _ = receiver.await;
    }
}

async fn await_actor_response<T, C>(
    receiver: futures::channel::oneshot::Receiver<T>,
    cancellation: &mut C,
) -> Result<T, ()>
where
    C: Future<Output = ()> + Unpin,
{
    let mut receiver = receiver;
    futures::future::poll_fn(|context| {
        if Pin::new(&mut *cancellation).poll(context).is_ready() {
            return Poll::Ready(Err(()));
        }
        match Pin::new(&mut receiver).poll(context) {
            Poll::Ready(Ok(value)) => Poll::Ready(Ok(value)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(())),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

async fn serve_socket_worker(
    stream: tokio::net::TcpStream,
    actor: futures::channel::mpsc::UnboundedSender<ActorCommand>,
    registry: Rc<RefCell<WorkerRegistry>>,
    worker_id: u64,
    cancellation_receiver: futures::channel::oneshot::Receiver<()>,
) {
    let (reader, writer) = stream.into_split();
    let reader = TokioReader(reader);
    let mut reader = PrefixedReader::new(reader);
    let writer = TokioWriter(writer);
    let mut cancellation = cancellation_receiver.map(|_| ());
    if let Ok(initial) = read_initial_request(&mut reader, &mut cancellation).await
        && let Some(parsed) = parse_http_request(&initial, TransportLimits::default())
            .ok()
            .flatten()
    {
        let request = parsed.request().clone();
        if request.method == "GET" && request.path.starts_with("/orna/live/") {
            serve_websocket_worker(
                reader,
                writer,
                initial,
                actor,
                registry,
                worker_id,
                &mut cancellation,
            )
            .await;
        } else {
            serve_http_worker(reader, writer, initial, actor, registry, &mut cancellation).await;
        }
    }
}

async fn serve_http_worker<C>(
    mut reader: PrefixedReader<TokioReader>,
    mut writer: TokioWriter,
    initial: Vec<u8>,
    actor: futures::channel::mpsc::UnboundedSender<ActorCommand>,
    registry: Rc<RefCell<WorkerRegistry>>,
    cancellation: &mut C,
) where
    C: Future<Output = ()> + Unpin,
{
    let mut connection = HttpConnection::new(TransportLimits::default());
    if serve_http_bytes(
        &mut connection,
        &initial,
        &actor,
        &registry,
        &mut writer,
        cancellation,
    )
    .await
    .is_err()
    {
        return;
    }
    let mut chunk = [0; 8192];
    loop {
        let count = match await_socket_io(reader.read(&mut chunk), cancellation).await {
            Ok(count) => count,
            Err(()) => return,
        };
        if count == 0 {
            return;
        }
        if serve_http_bytes(
            &mut connection,
            &chunk[..count],
            &actor,
            &registry,
            &mut writer,
            cancellation,
        )
        .await
        .is_err()
        {
            return;
        }
    }
}

async fn serve_http_bytes<C>(
    connection: &mut HttpConnection,
    bytes: &[u8],
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    registry: &Rc<RefCell<WorkerRegistry>>,
    writer: &mut TokioWriter,
    cancellation: &mut C,
) -> Result<(), ()>
where
    C: Future<Output = ()> + Unpin,
{
    let (returned, responses, retired) =
        actor_http(actor, connection.clone(), bytes, cancellation).await?;
    *connection = returned;
    retire_and_join(registry, retired).await;
    for response in responses {
        await_socket_io(writer.write_all(&response), cancellation).await?;
        await_socket_io(writer.flush(), cancellation).await?;
    }
    Ok(())
}

async fn actor_http(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    connection: HttpConnection,
    bytes: &[u8],
    cancellation: &mut (impl Future<Output = ()> + Unpin),
) -> Result<(HttpConnection, Vec<Vec<u8>>, Vec<[u8; 16]>), ()> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    actor
        .unbounded_send(ActorCommand::Http {
            connection,
            bytes: bytes.to_vec(),
            reply: sender,
        })
        .map_err(|_| ())?;
    let result = await_actor_response(receiver, cancellation)
        .await?
        .map_err(|_| ())?;
    Ok((result.connection, result.responses, result.retired))
}

async fn serve_websocket_worker<C>(
    mut reader: PrefixedReader<TokioReader>,
    mut writer: TokioWriter,
    initial: Vec<u8>,
    actor: futures::channel::mpsc::UnboundedSender<ActorCommand>,
    registry: Rc<RefCell<WorkerRegistry>>,
    worker_id: u64,
    cancellation: &mut C,
) where
    C: Future<Output = ()> + Unpin,
{
    let Some(parsed) = parse_http_request(&initial, TransportLimits::default())
        .ok()
        .flatten()
    else {
        return;
    };
    let request = parsed.request().clone();
    let remainder = initial[parsed.consumed()..].to_vec();
    let attachment = match opaque_attachment() {
        Ok(attachment) => attachment,
        Err(()) => return,
    };
    let prepared = match actor_prepare(&actor, request, attachment, cancellation).await {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(response)) => {
            let Ok(encoded) = response.encode_http(TransportLimits::default()) else {
                return;
            };
            let _ = await_socket_io(writer.write_all(&encoded), cancellation).await;
            let _ = await_socket_io(writer.flush(), cancellation).await;
            return;
        }
        Err(()) => return,
    };
    let response = prepared.response().clone();
    let encoded = match response.encode_http(TransportLimits::default()) {
        Ok(encoded) => encoded,
        Err(_) => return,
    };
    if await_socket_io(writer.write_all(&encoded), cancellation)
        .await
        .is_err()
        || await_socket_io(writer.flush(), cancellation).await.is_err()
    {
        return;
    }
    if response.status != 101 {
        return;
    }
    registry.borrow_mut().attach(attachment, worker_id);
    let retired = match actor_commit(&actor, prepared, cancellation).await {
        Ok(retired) => retired,
        Err(()) => {
            registry.borrow_mut().detach(attachment, worker_id);
            return;
        }
    };
    retire_and_join(&registry, retired).await;
    let mut socket = WebSocketState::new(attachment);
    if !remainder.is_empty() {
        match serve_websocket_bytes(&actor, &mut socket, &remainder, &mut writer, cancellation)
            .await
        {
            Ok(false) => {}
            Ok(true) | Err(()) => {
                close_worker_attachment(&actor, &registry, attachment, worker_id, cancellation)
                    .await;
                return;
            }
        }
    }
    let mut chunk = [0; 8192];
    loop {
        let count = match await_socket_io(reader.read(&mut chunk), cancellation).await {
            Ok(count) => count,
            Err(()) => break,
        };
        if count == 0 {
            break;
        }
        match serve_websocket_bytes(
            &actor,
            &mut socket,
            &chunk[..count],
            &mut writer,
            cancellation,
        )
        .await
        {
            Ok(false) => {}
            Ok(true) | Err(()) => break,
        }
    }
    close_worker_attachment(&actor, &registry, attachment, worker_id, cancellation).await;
}

async fn actor_prepare(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    request: orna_live_v1::WireRequest,
    attachment: [u8; 16],
    cancellation: &mut (impl Future<Output = ()> + Unpin),
) -> Result<Result<orna_live_v1::WebSocketUpgrade, orna_live_v1::WireResponse>, ()> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    actor
        .unbounded_send(ActorCommand::Prepare {
            request,
            attachment,
            now: system_milliseconds(),
            reply: sender,
        })
        .map_err(|_| ())?;
    await_actor_response(receiver, cancellation).await
}

async fn actor_commit(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    upgrade: orna_live_v1::WebSocketUpgrade,
    cancellation: &mut (impl Future<Output = ()> + Unpin),
) -> Result<Vec<[u8; 16]>, ()> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    actor
        .unbounded_send(ActorCommand::Commit {
            upgrade,
            reply: sender,
        })
        .map_err(|_| ())?;
    let (_, retired) = await_actor_response(receiver, cancellation)
        .await?
        .map_err(|_| ())?;
    Ok(retired)
}

async fn serve_websocket_bytes<C>(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    socket: &mut WebSocketState,
    bytes: &[u8],
    writer: &mut TokioWriter,
    cancellation: &mut C,
) -> Result<bool, ()>
where
    C: Future<Output = ()> + Unpin,
{
    let (sender, receiver) = futures::channel::oneshot::channel();
    actor
        .unbounded_send(ActorCommand::Receive {
            socket: std::mem::replace(socket, WebSocketState::new([0; 16])),
            bytes: bytes.to_vec(),
            now: system_milliseconds(),
            reply: sender,
        })
        .map_err(|_| ())?;
    let (returned, outputs) = await_actor_response(receiver, cancellation)
        .await?
        .map_err(|_| ())?;
    *socket = returned;
    for output in outputs {
        let closing = matches!(output, WebSocketOutput::Close);
        let Some(frame) =
            encode_websocket_output(&output, TransportLimits::default()).map_err(|_| ())?
        else {
            continue;
        };
        await_socket_io(writer.write_all(&frame), cancellation).await?;
        await_socket_io(writer.flush(), cancellation).await?;
        if closing {
            await_socket_io(writer.close(), cancellation).await?;
            return Ok(true);
        }
    }
    Ok(false)
}

async fn close_worker_attachment(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    registry: &Rc<RefCell<WorkerRegistry>>,
    attachment: [u8; 16],
    worker_id: u64,
    cancellation: &mut (impl Future<Output = ()> + Unpin),
) {
    let (sender, receiver) = futures::channel::oneshot::channel();
    if actor
        .unbounded_send(ActorCommand::Close {
            attachment,
            now: system_milliseconds(),
            reply: sender,
        })
        .is_ok()
    {
        let _ = await_actor_response(receiver, cancellation).await;
    }
    registry.borrow_mut().detach(attachment, worker_id);
}

async fn await_socket_io<T, F, C>(operation: F, cancellation: &mut C) -> Result<T, ()>
where
    F: Future<Output = io::Result<T>> + Unpin,
    C: Future<Output = ()> + Unpin,
{
    let mut operation = operation;
    futures::future::poll_fn(|context| {
        if Pin::new(&mut *cancellation).poll(context).is_ready() {
            return Poll::Ready(Err(()));
        }
        Pin::new(&mut operation).poll(context).map_err(|_| ())
    })
    .await
}

fn map_connection_error(error: HttpIoError) -> LiveHostError {
    match error {
        HttpIoError::Cancelled => LiveHostError::Cancelled,
        _ => LiveHostError::Connection,
    }
}

async fn read_initial_request<C>(
    reader: &mut (impl futures::io::AsyncRead + Unpin),
    cancellation: &mut C,
) -> Result<Vec<u8>, LiveHostError>
where
    C: Future<Output = ()> + Unpin,
{
    let limits = TransportLimits::default();
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        match parse_http_request(&bytes, limits) {
            Ok(Some(_)) => return Ok(bytes),
            Ok(None) => {}
            Err(_) => return Err(LiveHostError::Connection),
        }
        let mut read = reader.read(&mut chunk);
        let count = futures::future::poll_fn(|context| {
            if Pin::new(&mut *cancellation).poll(context).is_ready() {
                return Poll::Ready(Err(LiveHostError::Cancelled));
            }
            Pin::new(&mut read)
                .poll(context)
                .map(|result| result.map_err(|_| LiveHostError::Connection))
        })
        .await?;
        if count == 0 {
            return Err(LiveHostError::Connection);
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

fn opaque_attachment() -> Result<[u8; 16], ()> {
    let mut attachment = [0; 16];
    for _ in 0..8 {
        getrandom::fill(&mut attachment).map_err(|_| ())?;
        if attachment != [0; 16] {
            return Ok(attachment);
        }
    }
    Err(())
}

struct RejectLiveApplication;

impl LiveApplication for RejectLiveApplication {
    fn eval(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> orna_live_v1::Result<Envelope> {
        Err(orna_live_v1::Error::UnsupportedOperation)
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> orna_live_v1::Result<Envelope> {
        Err(orna_live_v1::Error::UnsupportedOperation)
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

struct PrefixedReader<R> {
    prefix: Vec<u8>,
    offset: usize,
    reader: R,
}

impl<R> PrefixedReader<R> {
    fn new(reader: R) -> Self {
        Self {
            prefix: Vec::new(),
            offset: 0,
            reader,
        }
    }

    fn replay(&mut self, prefix: Vec<u8>) {
        self.prefix = prefix;
        self.offset = 0;
    }
}

impl<R> futures::io::AsyncRead for PrefixedReader<R>
where
    R: futures::io::AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.offset < self.prefix.len() {
            let count = (self.prefix.len() - self.offset).min(buffer.len());
            buffer[..count].copy_from_slice(&self.prefix[self.offset..self.offset + count]);
            self.offset += count;
            return Poll::Ready(Ok(count));
        }
        Pin::new(&mut self.reader).poll_read(context, buffer)
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

fn system_milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, duration_milliseconds)
}

fn duration_milliseconds(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::duration_milliseconds;
    use std::time::Duration;

    #[test]
    fn live_clock_uses_milliseconds_for_the_advertised_lease() {
        assert_eq!(duration_milliseconds(Duration::from_secs(30)), 30_000);
    }
}
