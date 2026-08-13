//! Authenticated local raw-call connection handling.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    future::Future,
    io::{self, Write},
    net::Shutdown,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener as StandardUnixListener, UnixStream as StandardUnixStream},
    },
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use orna_core::{
    catalogue::CatalogueSnapshot, revision::ActiveDatabaseRevision, security::AuthenticatedSession,
    value::OpaqueCodecRegistry,
};
use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};
use orna_protocol::{
    ClientAction, ClientFrame, ConnectionError, FrameCodecError, MAX_FRAME_PAYLOAD_LENGTH,
    ProtocolConnection, RawCall, ServerAction, ServerFrame, decode_active_client_frame,
    decode_catalogue_client_frame, decode_client_frame, decode_registered_client_frame,
    encode_active_server_frame, encode_catalogue_server_frame, encode_registered_server_frame,
    encode_server_frame,
};
use orna_standard::{RegisteredOpaqueCodecsError, registered_opaque_codecs};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::{JoinError, JoinSet},
    time::{Instant, timeout_at},
};

use crate::{RawClientDispatch, authenticate_local_stream};

const CLIENT_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00";
const CLIENT_CATALOGUE_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x02\x00\x00\x00\x00";
const CLIENT_ACTIVE_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00";
const CLIENT_REGISTERED_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x04\x00\x00\x00\x00";
const SERVER_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00";
const SERVER_CATALOGUE_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x02\x00\x00\x00\x00";
const SERVER_ACTIVE_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00";
const SERVER_REGISTERED_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x04\x00\x00\x00\x00";
const FRAME_HEADER_LENGTH: usize = 18;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const SHARED_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;
const KERNEL_OPERATION_LIMIT: usize = 64;
const CONNECTION_LIMIT: usize = 64;
const SOCKET_NAME: &str = "orna.sock";

#[derive(Clone)]
enum RawProtocolVersion {
    One,
    Catalogue(Arc<CatalogueSnapshot>),
    Active(Arc<ActiveDatabaseRevision>),
    Registered(Arc<ActiveDatabaseRevision>, Arc<OpaqueCodecRegistry>),
}

impl RawProtocolVersion {
    fn decode_client_frame(&self, encoded: &[u8]) -> Result<ClientFrame, FrameCodecError> {
        match self {
            Self::One => decode_client_frame(encoded),
            Self::Catalogue(catalogue) => decode_catalogue_client_frame(catalogue, encoded),
            Self::Active(active) => decode_active_client_frame(active, encoded),
            Self::Registered(active, registry) => {
                decode_registered_client_frame(active, registry, encoded)
            }
        }
    }

    fn encode_server_frame(&self, frame: &ServerFrame) -> Result<Vec<u8>, FrameCodecError> {
        match self {
            Self::One => encode_server_frame(frame),
            Self::Catalogue(catalogue) => encode_catalogue_server_frame(catalogue, frame),
            Self::Active(active) => encode_active_server_frame(active, frame),
            Self::Registered(active, registry) => {
                encode_registered_server_frame(active, registry, frame)
            }
        }
    }

    fn receive(
        &self,
        connection: &mut ProtocolConnection,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        match self {
            Self::One => connection.receive(frame),
            Self::Catalogue(catalogue) => connection.receive_catalogue(catalogue, frame),
            Self::Active(active) => connection.receive_active(active, frame),
            Self::Registered(active, registry) => {
                connection.receive_registered(active, registry, frame)
            }
        }
    }

    fn apply(
        &self,
        connection: &mut ProtocolConnection,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        match self {
            Self::One => connection.apply(action),
            Self::Catalogue(catalogue) => connection.apply_catalogue(catalogue, action),
            Self::Active(active) => connection.apply_active(active, action),
            Self::Registered(active, registry) => {
                connection.apply_registered(active, registry, action)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestedProtocol {
    One,
    Catalogue,
    Active,
    Registered,
}

impl RequestedProtocol {
    const fn acknowledgement(self) -> &'static [u8; 12] {
        match self {
            Self::One => &SERVER_ACK,
            Self::Catalogue => &SERVER_CATALOGUE_ACK,
            Self::Active => &SERVER_ACTIVE_ACK,
            Self::Registered => &SERVER_REGISTERED_ACK,
        }
    }
}

fn requested_protocol(hello: &[u8; 12]) -> Option<RequestedProtocol> {
    match *hello {
        CLIENT_HELLO => Some(RequestedProtocol::One),
        CLIENT_CATALOGUE_HELLO => Some(RequestedProtocol::Catalogue),
        CLIENT_ACTIVE_HELLO => Some(RequestedProtocol::Active),
        CLIENT_REGISTERED_HELLO => Some(RequestedProtocol::Registered),
        _ => None,
    }
}

/// Listener-wide admission resources for authenticated local raw calls.
#[derive(Clone)]
pub struct LocalRawSocketResources {
    payload: Arc<Semaphore>,
    kernel_operations: Arc<Semaphore>,
}

impl LocalRawSocketResources {
    /// Creates the fixed listener budgets.
    pub fn new() -> Self {
        Self {
            payload: Arc::new(Semaphore::new(SHARED_PAYLOAD_BYTES)),
            kernel_operations: Arc::new(Semaphore::new(KERNEL_OPERATION_LIMIT)),
        }
    }

    fn reserve_payload(&self, length: usize) -> Result<PayloadReservation, LocalRawSocketError> {
        let permits = u32::try_from(length).expect("frame limit fits semaphore permits");
        let permit = if permits == 0 {
            None
        } else {
            Some(
                Arc::clone(&self.payload)
                    .try_acquire_many_owned(permits)
                    .map_err(|_| LocalRawSocketError::PayloadCapacity)?,
            )
        };
        Ok(PayloadReservation { _permit: permit })
    }

    fn reserve_kernel_operation(&self) -> Result<OwnedSemaphorePermit, LocalRawSocketError> {
        Arc::clone(&self.kernel_operations)
            .try_acquire_owned()
            .map_err(|_| LocalRawSocketError::KernelCapacity)
    }
}

impl Default for LocalRawSocketResources {
    fn default() -> Self {
        Self::new()
    }
}

/// A trusted failure from one local raw socket connection.
#[derive(Debug)]
#[non_exhaustive]
pub enum LocalRawSocketError {
    /// The exact client hello was not completed before its deadline.
    HandshakeTimeout,
    /// The client hello did not select the exact supported protocol.
    InvalidHello,
    /// The client supplied no complete next frame before the idle deadline.
    FrameTimeout,
    /// The shared payload-byte budget could not admit a declared frame.
    PayloadCapacity,
    /// The shared kernel-operation budget could not admit authentication or dispatch.
    KernelCapacity,
    /// The connected operating-system peer could not authenticate.
    Authentication {
        /// The protected authentication failure.
        source: crate::LocalAuthenticationError,
    },
    /// The authenticated version-2 connection could not recover its catalogue.
    Catalogue {
        /// The protected catalogue recovery failure.
        source: Box<PostgresKernelError>,
    },
    /// The authenticated version-3 connection could not recover its active revision.
    ActiveRevision {
        /// The protected active-revision recovery failure.
        source: Box<PostgresKernelError>,
    },
    /// The authenticated version-4 connection could not bind its opaque registry.
    OpaqueRegistry {
        /// The checked-in opaque codec registry failure.
        source: RegisteredOpaqueCodecsError,
    },
    /// Socket I/O failed or ended within a required envelope.
    Io {
        /// The socket failure.
        source: io::Error,
    },
    /// A client frame violated the closed wire format.
    Frame {
        /// The frame codec failure.
        source: FrameCodecError,
    },
    /// A decoded frame or server action violated the connection state machine.
    Connection {
        /// The state-machine failure.
        source: ConnectionError,
    },
    /// One protected dispatch task failed outside its typed result.
    DispatchTask {
        /// The unexpected task failure.
        source: JoinError,
    },
    /// The independently owned connection task failed outside its typed result.
    ConnectionTask {
        /// The unexpected task failure.
        source: JoinError,
    },
}

impl fmt::Display for LocalRawSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::HandshakeTimeout => "local raw socket handshake timed out",
            Self::InvalidHello => "local raw socket handshake is invalid",
            Self::FrameTimeout => "local raw socket frame timed out",
            Self::PayloadCapacity => "local raw socket payload capacity is exhausted",
            Self::KernelCapacity => "local raw socket kernel capacity is exhausted",
            Self::Authentication { .. } => "local raw socket authentication failed",
            Self::Catalogue { .. } => "local raw socket catalogue recovery failed",
            Self::ActiveRevision { .. } => "local raw socket active revision recovery failed",
            Self::OpaqueRegistry { .. } => "local raw socket opaque registry validation failed",
            Self::Io { .. } => "local raw socket I/O failed",
            Self::Frame { .. } => "local raw socket frame is invalid",
            Self::Connection { .. } => "local raw socket state is invalid",
            Self::DispatchTask { .. } => "local raw socket dispatch task failed",
            Self::ConnectionTask { .. } => "local raw socket connection task failed",
        })
    }
}

impl Error for LocalRawSocketError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Authentication { source } => Some(source),
            Self::Catalogue { source } => Some(source),
            Self::ActiveRevision { source } => Some(source),
            Self::OpaqueRegistry { source } => Some(source),
            Self::Io { source } => Some(source),
            Self::Frame { source } => Some(source),
            Self::Connection { source } => Some(source),
            Self::DispatchTask { source } => Some(source),
            Self::ConnectionTask { source } => Some(source),
            Self::HandshakeTimeout
            | Self::InvalidHello
            | Self::FrameTimeout
            | Self::PayloadCapacity
            | Self::KernelCapacity => None,
        }
    }
}

impl LocalRawSocketError {
    const fn is_infrastructure_failure(&self) -> bool {
        matches!(
            self,
            Self::DispatchTask { .. } | Self::ConnectionTask { .. }
        )
    }
}

/// A failure in the fixed local raw-socket listener lifecycle.
#[derive(Debug)]
#[non_exhaustive]
pub enum LocalRawSocketServerError {
    /// The fixed runtime directory or public socket has hostile metadata.
    InvalidSocketState,
    /// The listener or one connection worker failed outside a client error.
    Infrastructure {
        /// The trusted listener or worker failure.
        source: LocalRawSocketError,
    },
    /// The dedicated listener thread panicked.
    ListenerThread,
    /// A host socket or filesystem operation failed.
    Io {
        /// The host I/O failure.
        source: io::Error,
    },
}

impl fmt::Display for LocalRawSocketServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidSocketState => "local raw socket state is invalid",
            Self::Infrastructure { .. } => "local raw socket infrastructure failed",
            Self::ListenerThread => "local raw socket listener thread failed",
            Self::Io { .. } => "local raw socket host I/O failed",
        })
    }
}

impl Error for LocalRawSocketServerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Infrastructure { source } => Some(source),
            Self::Io { source } => Some(source),
            Self::InvalidSocketState | Self::ListenerThread => None,
        }
    }
}

impl From<io::Error> for LocalRawSocketServerError {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

/// One verified fixed local raw listener and all of its connection workers.
pub struct LocalRawSocketServer {
    shutdown: watch::Sender<bool>,
    thread: Option<thread::JoinHandle<Result<(), LocalRawSocketServerError>>>,
    healthy: Arc<AtomicBool>,
    socket_path: PathBuf,
    socket_present: bool,
    uid: u32,
    gid: u32,
}

struct BoundSocketCleanup {
    path: PathBuf,
    active: bool,
}

impl Drop for BoundSocketCleanup {
    fn drop(&mut self) {
        if self.active {
            let _ = std::fs::remove_file(&self.path);
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::File::open(parent).and_then(|directory| directory.sync_all());
            }
        }
    }
}

impl LocalRawSocketServer {
    /// Reports whether the listener thread remains live.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
            && self
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
    }

    /// Stops acceptance, drains every connection, and removes the public socket.
    ///
    /// # Errors
    ///
    /// Returns a typed listener, worker, socket-state, or host I/O failure after
    /// attempting the complete ordered cleanup.
    pub fn stop(mut self) -> Result<(), LocalRawSocketServerError> {
        let result = self.stop_inner();
        self.thread = None;
        result
    }

    fn stop_inner(&mut self) -> Result<(), LocalRawSocketServerError> {
        let _ = self.shutdown.send(true);
        let listener = match self.thread.take() {
            Some(thread) => match thread.join() {
                Ok(result) => result,
                Err(_) => Err(LocalRawSocketServerError::ListenerThread),
            },
            None => Ok(()),
        };
        let removal = if self.socket_present {
            let removal = remove_verified_socket(&self.socket_path, self.uid, self.gid);
            if removal.is_ok() {
                self.socket_present = false;
            }
            removal
        } else {
            Ok(())
        };
        listener?;
        removal
    }
}

impl Drop for LocalRawSocketServer {
    fn drop(&mut self) {
        let _ = self.stop_inner();
    }
}

/// Starts the fixed `orna.sock` listener below a verified runtime directory.
///
/// The caller must hold the exclusive instance lock for this handle's complete
/// lifetime. No supplied value can select the socket filename, identity,
/// protocol limits, or dispatch authority.
///
/// # Errors
///
/// Returns a typed socket-state, host I/O, or listener-thread startup failure.
pub fn start_local_raw_socket(
    runtime_directory: &Path,
    kernel: PostgresKernel,
) -> Result<LocalRawSocketServer, LocalRawSocketServerError> {
    // SAFETY: these calls only read the current process credentials.
    let uid = unsafe { nix::libc::geteuid() };
    // SAFETY: these calls only read the current process credentials.
    let gid = unsafe { nix::libc::getegid() };
    require_runtime_directory(runtime_directory, uid, gid)?;
    let socket_path = runtime_directory.join(SOCKET_NAME);
    remove_stale_socket(&socket_path, uid, gid)?;
    let listener = StandardUnixListener::bind(&socket_path)?;
    let mut cleanup = BoundSocketCleanup {
        path: socket_path.clone(),
        active: true,
    };
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o666))?;
    require_socket(&socket_path, uid, gid)?;
    sync_directory(runtime_directory)?;
    listener.set_nonblocking(true)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let listener = {
        let _runtime_context = runtime.enter();
        tokio::net::UnixListener::from_std(listener)?
    };
    let (shutdown, shutdown_receiver) = watch::channel(false);
    let resources = LocalRawSocketResources::new();
    let listener_shutdown = shutdown.clone();
    let healthy = Arc::new(AtomicBool::new(true));
    let listener_health = Arc::clone(&healthy);
    let listener_thread = thread::Builder::new()
        .name("orna-raw-socket".to_owned())
        .spawn(move || {
            runtime.block_on(run_listener(
                listener,
                kernel,
                resources,
                listener_shutdown,
                shutdown_receiver,
                listener_health,
            ))
        })?;
    cleanup.active = false;
    Ok(LocalRawSocketServer {
        shutdown,
        thread: Some(listener_thread),
        healthy,
        socket_path,
        socket_present: true,
        uid,
        gid,
    })
}

async fn run_listener(
    listener: tokio::net::UnixListener,
    kernel: PostgresKernel,
    resources: LocalRawSocketResources,
    shutdown_signal: watch::Sender<bool>,
    mut shutdown: watch::Receiver<bool>,
    healthy: Arc<AtomicBool>,
) -> Result<(), LocalRawSocketServerError> {
    let connections = Arc::new(Semaphore::new(CONNECTION_LIMIT));
    let mut workers = JoinSet::new();
    let mut result = loop {
        tokio::select! {
            _ = wait_for_shutdown(&mut shutdown) => break Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(source) => break Err(source.into()),
                };
                let Ok(connection_permit) = Arc::clone(&connections).try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let stream = match stream.into_std() {
                    Ok(stream) => stream,
                    Err(source) => break Err(source.into()),
                };
                let kernel = kernel.clone();
                let resources = resources.clone();
                let connection_shutdown = shutdown.clone();
                workers.spawn(async move {
                    let _connection_permit = connection_permit;
                    serve_local_raw_stream_until_shutdown(
                        kernel,
                        stream,
                        resources,
                        connection_shutdown,
                    )
                    .await
                });
            }
            completed = workers.join_next(), if !workers.is_empty() => {
                match completed {
                    Some(Ok(Err(source))) if source.is_infrastructure_failure() => {
                        break Err(LocalRawSocketServerError::Infrastructure { source });
                    }
                    Some(Err(source)) => {
                        break Err(LocalRawSocketServerError::Infrastructure {
                            source: LocalRawSocketError::ConnectionTask { source },
                        });
                    }
                    Some(Ok(Ok(()) | Err(_))) | None => {}
                }
            }
        }
    };

    if result.is_err() {
        let _ = shutdown_signal.send(true);
    }
    healthy.store(false, Ordering::SeqCst);
    drop(listener);

    while let Some(completed) = workers.join_next().await {
        match completed {
            Ok(Err(source)) if source.is_infrastructure_failure() && result.is_ok() => {
                result = Err(LocalRawSocketServerError::Infrastructure { source });
            }
            Err(source) if result.is_ok() => {
                result = Err(LocalRawSocketServerError::Infrastructure {
                    source: LocalRawSocketError::ConnectionTask { source },
                });
            }
            Ok(Ok(()) | Err(_)) | Err(_) => {}
        }
    }
    result
}

fn require_runtime_directory(
    path: &Path,
    uid: u32,
    gid: u32,
) -> Result<(), LocalRawSocketServerError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.mode() & 0o7777 != 0o711
    {
        return Err(LocalRawSocketServerError::InvalidSocketState);
    }
    Ok(())
}

fn require_socket(path: &Path, uid: u32, gid: u32) -> Result<(), LocalRawSocketServerError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.mode() & 0o7777 != 0o666
        || metadata.nlink() != 1
    {
        return Err(LocalRawSocketServerError::InvalidSocketState);
    }
    Ok(())
}

fn remove_stale_socket(path: &Path, uid: u32, gid: u32) -> Result<(), LocalRawSocketServerError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            require_socket(path, uid, gid)?;
            std::fs::remove_file(path)?;
            sync_directory(
                path.parent()
                    .ok_or(LocalRawSocketServerError::InvalidSocketState)?,
            )
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source.into()),
    }
}

fn remove_verified_socket(
    path: &Path,
    uid: u32,
    gid: u32,
) -> Result<(), LocalRawSocketServerError> {
    require_socket(path, uid, gid)?;
    std::fs::remove_file(path)?;
    sync_directory(
        path.parent()
            .ok_or(LocalRawSocketServerError::InvalidSocketState)?,
    )
}

fn sync_directory(path: &Path) -> Result<(), LocalRawSocketServerError> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// Negotiates, authenticates, and drives one connected local raw stream.
///
/// The actual Unix peer is the sole authentication input. The supplied stream
/// must already be connected; no request bytes can select session identity.
/// The operation transfers the connection to an independently owned task
/// before it first awaits. Dropping this returned future stops waiting but does
/// not cancel authentication, accepted dispatch, or ordered connection drain.
///
/// # Errors
///
/// Returns [`LocalRawSocketError`] for handshake, authentication, active-state
/// recovery, opaque-registry validation, capacity, I/O, codec, state-machine,
/// or protected task failures. No error text is written to the client.
pub async fn serve_local_raw_stream(
    kernel: PostgresKernel,
    stream: StandardUnixStream,
    resources: LocalRawSocketResources,
) -> Result<(), LocalRawSocketError> {
    let (shutdown_guard, shutdown) = watch::channel(false);
    run_owned_connection_with_shutdown_guard(shutdown_guard, async move {
        negotiate_and_drive(kernel, stream, resources, shutdown).await
    })
    .await
}

pub(super) async fn serve_local_raw_stream_until_shutdown(
    kernel: PostgresKernel,
    stream: StandardUnixStream,
    resources: LocalRawSocketResources,
    shutdown: watch::Receiver<bool>,
) -> Result<(), LocalRawSocketError> {
    run_owned_connection(async move {
        let shutdown_stream = stream
            .try_clone()
            .map_err(|source| LocalRawSocketError::Io { source })?;
        let mut socket_shutdown = shutdown.clone();
        let shutdown_task = tokio::spawn(async move {
            wait_for_shutdown(&mut socket_shutdown).await;
            let _ = shutdown_stream.shutdown(Shutdown::Both);
        });
        let result = negotiate_and_drive(kernel, stream, resources, shutdown).await;
        shutdown_task.abort();
        let _ = shutdown_task.await;
        result
    })
    .await
}

async fn run_owned_connection<F>(connection: F) -> Result<(), LocalRawSocketError>
where
    F: Future<Output = Result<(), LocalRawSocketError>> + Send + 'static,
{
    tokio::spawn(connection)
        .await
        .map_err(|source| LocalRawSocketError::ConnectionTask { source })?
}

async fn run_owned_connection_with_shutdown_guard<F>(
    shutdown_guard: watch::Sender<bool>,
    connection: F,
) -> Result<(), LocalRawSocketError>
where
    F: Future<Output = Result<(), LocalRawSocketError>> + Send + 'static,
{
    run_owned_connection(async move {
        let _shutdown_guard = shutdown_guard;
        connection.await
    })
    .await
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow_and_update() {
            return;
        }
        if shutdown.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn negotiate_and_drive(
    kernel: PostgresKernel,
    stream: StandardUnixStream,
    resources: LocalRawSocketResources,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), LocalRawSocketError> {
    let peer_stream = stream
        .try_clone()
        .map_err(|source| LocalRawSocketError::Io { source })?;
    stream
        .set_nonblocking(true)
        .map_err(|source| LocalRawSocketError::Io { source })?;
    let mut stream =
        UnixStream::from_std(stream).map_err(|source| LocalRawSocketError::Io { source })?;

    let mut hello = [0_u8; CLIENT_HELLO.len()];
    if *shutdown.borrow() {
        return Ok(());
    }
    tokio::select! {
        result = read_exact_before(
            &mut stream,
            &mut hello,
            Instant::now() + HANDSHAKE_TIMEOUT,
            LocalRawSocketError::HandshakeTimeout,
        ) => result?,
        _ = wait_for_shutdown(&mut shutdown) => return Ok(()),
    }
    let requested = requested_protocol(&hello).ok_or(LocalRawSocketError::InvalidHello)?;

    let authentication_permit = resources.reserve_kernel_operation()?;
    let session = authenticate_local_stream(&kernel, &peer_stream)
        .await
        .map_err(|source| LocalRawSocketError::Authentication { source })?;
    let version =
        match requested {
            RequestedProtocol::One => RawProtocolVersion::One,
            RequestedProtocol::Catalogue => {
                let active =
                    kernel
                        .recover()
                        .await
                        .map_err(|source| LocalRawSocketError::Catalogue {
                            source: Box::new(source),
                        })?;
                RawProtocolVersion::Catalogue(Arc::new(active.catalogue().clone()))
            }
            RequestedProtocol::Active => {
                let active = kernel.recover().await.map_err(|source| {
                    LocalRawSocketError::ActiveRevision {
                        source: Box::new(source),
                    }
                })?;
                RawProtocolVersion::Active(Arc::new(active))
            }
            RequestedProtocol::Registered => {
                let active = Arc::new(kernel.recover().await.map_err(|source| {
                    LocalRawSocketError::ActiveRevision {
                        source: Box::new(source),
                    }
                })?);
                let standard = active.catalogue_hash_context().standard().ok_or(
                    LocalRawSocketError::OpaqueRegistry {
                        source: RegisteredOpaqueCodecsError::UnacceptedStandardSnapshot,
                    },
                )?;
                let registry = registered_opaque_codecs(standard)
                    .map_err(|source| LocalRawSocketError::OpaqueRegistry { source })?;
                RawProtocolVersion::Registered(active, Arc::new(registry))
            }
        };
    drop(authentication_permit);
    if *shutdown.borrow() {
        return Ok(());
    }
    let acknowledgement = requested.acknowledgement();
    if !write_all_until_shutdown(&mut stream, acknowledgement, &mut shutdown).await? {
        return Ok(());
    }

    drive_versioned_authenticated_stream_until_shutdown(
        RawDispatchService { kernel },
        session,
        version,
        stream,
        resources,
        shutdown,
    )
    .await
}

struct PayloadReservation {
    _permit: Option<OwnedSemaphorePermit>,
}

struct DispatchGuards {
    _operation: OwnedSemaphorePermit,
    _payload: Vec<PayloadReservation>,
}

struct IncomingFrame {
    frame: ClientFrame,
    reservation: PayloadReservation,
}

struct DispatchCompletion {
    actions: VecDeque<ServerAction>,
    cancellation: ServerAction,
    _guards: Option<DispatchGuards>,
}

struct StartedDispatch {
    accepted: ServerAction,
    future: DispatchFuture,
}

type DispatchFuture = Pin<Box<dyn Future<Output = DispatchCompletion> + Send>>;

trait DispatchService: Clone + Send + Sync + 'static {
    fn start(&self, session: AuthenticatedSession, stream: u64, call: RawCall) -> StartedDispatch;

    fn cancelled(&self, _stream: u64) {}
}

#[derive(Clone)]
struct RawDispatchService {
    kernel: PostgresKernel,
}

impl DispatchService for RawDispatchService {
    fn start(&self, session: AuthenticatedSession, stream: u64, call: RawCall) -> StartedDispatch {
        let dispatch = RawClientDispatch::new(self.kernel.clone(), session, stream, call);
        let accepted = dispatch.accepted_action();
        let future = Box::pin(async move {
            let result = dispatch.finish().await;
            let cancellation = result.action_after_cancellation();
            if let Some(source) = result.source() {
                report_private_dispatch_source(source);
            }
            DispatchCompletion {
                actions: result.into_actions().into(),
                cancellation,
                _guards: None,
            }
        });
        StartedDispatch { accepted, future }
    }
}

struct UnstartedDispatch {
    stream: u64,
    future: DispatchFuture,
    guards: DispatchGuards,
    defer_once: bool,
}

#[cfg(test)]
async fn drive_authenticated_stream<D: DispatchService>(
    dispatcher: D,
    session: AuthenticatedSession,
    stream: UnixStream,
    resources: LocalRawSocketResources,
) -> Result<(), LocalRawSocketError> {
    let (shutdown_guard, shutdown) = watch::channel(false);
    let result = drive_versioned_authenticated_stream_until_shutdown(
        dispatcher,
        session,
        RawProtocolVersion::One,
        stream,
        resources,
        shutdown,
    )
    .await;
    drop(shutdown_guard);
    result
}

#[cfg(test)]
async fn drive_authenticated_stream_until_shutdown<D: DispatchService>(
    dispatcher: D,
    session: AuthenticatedSession,
    stream: UnixStream,
    resources: LocalRawSocketResources,
    shutdown: watch::Receiver<bool>,
) -> Result<(), LocalRawSocketError> {
    drive_versioned_authenticated_stream_until_shutdown(
        dispatcher,
        session,
        RawProtocolVersion::One,
        stream,
        resources,
        shutdown,
    )
    .await
}

async fn drive_versioned_authenticated_stream_until_shutdown<D: DispatchService>(
    dispatcher: D,
    session: AuthenticatedSession,
    version: RawProtocolVersion,
    stream: UnixStream,
    resources: LocalRawSocketResources,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), LocalRawSocketError> {
    let (reader, mut writer) = stream.into_split();
    let (frame_sender, mut frame_receiver) = mpsc::channel(64);
    let reader_task = spawn_frame_reader(reader, version.clone(), resources.clone(), frame_sender);
    let mut connection = ProtocolConnection::new();
    let mut retained_payload = BTreeMap::<u64, Vec<PayloadReservation>>::new();
    let mut cancelled = BTreeSet::<u64>::new();
    let mut pending = BTreeMap::<u64, DispatchCompletion>::new();
    let mut tasks = JoinSet::<(u64, DispatchCompletion)>::new();
    let mut unstarted = VecDeque::<UnstartedDispatch>::new();
    let result = loop {
        match flush_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut writer,
            &mut shutdown,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => break Ok(()),
            Err(error) => break Err(error),
        }

        enum Next {
            Frame(Result<Option<IncomingFrame>, LocalRawSocketError>),
            Completion(Option<Result<(u64, DispatchCompletion), JoinError>>),
            Shutdown,
            Start,
        }

        if *shutdown.borrow() {
            break Ok(());
        }
        let next = if let Some(dispatch) = unstarted.front_mut() {
            if dispatch.defer_once {
                dispatch.defer_once = false;
                tokio::select! {
                    biased;
                    _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                    frame = frame_receiver.recv() => Next::Frame(frame.unwrap_or(Ok(None))),
                    () = tokio::task::yield_now() => Next::Start,
                }
            } else {
                Next::Start
            }
        } else if tasks.is_empty() {
            tokio::select! {
                _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                frame = frame_receiver.recv() => Next::Frame(frame.unwrap_or(Ok(None))),
            }
        } else {
            tokio::select! {
                _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                frame = frame_receiver.recv() => {
                    Next::Frame(frame.unwrap_or(Ok(None)))
                }
                completion = tasks.join_next(), if !tasks.is_empty() => {
                    Next::Completion(completion)
                }
            }
        };

        match next {
            Next::Frame(Ok(Some(incoming))) => {
                match handle_client_frame(
                    incoming,
                    &dispatcher,
                    &session,
                    &version,
                    &resources,
                    &mut connection,
                    &mut retained_payload,
                    &mut cancelled,
                    &mut pending,
                    &mut unstarted,
                    &mut writer,
                    &mut shutdown,
                )
                .await
                {
                    Ok(true) => {}
                    Ok(false) => break Ok(()),
                    Err(error) => break Err(error),
                }
            }
            Next::Frame(Ok(None)) => break Ok(()),
            Next::Frame(Err(error)) => break Err(error),
            Next::Completion(Some(Ok((stream_id, completion)))) => {
                let mut completion = completion;
                if cancelled.remove(&stream_id) {
                    completion.actions = VecDeque::from([completion.cancellation.clone()]);
                }
                pending.insert(stream_id, completion);
            }
            Next::Completion(Some(Err(source))) => {
                break Err(LocalRawSocketError::DispatchTask { source });
            }
            Next::Completion(None) => {}
            Next::Shutdown => break Ok(()),
            Next::Start => {
                start_one_dispatch(&mut unstarted, &mut tasks);
            }
        }
    };

    reader_task.abort();
    let _ = reader_task.await;
    while !unstarted.is_empty() {
        start_one_dispatch(&mut unstarted, &mut tasks);
    }
    let mut drain_failure = None;
    while let Some(completion) = tasks.join_next().await {
        if let Err(source) = completion {
            drain_failure.get_or_insert(LocalRawSocketError::DispatchTask { source });
        }
    }
    match (result, drain_failure) {
        (Err(error), _) => Err(error),
        (Ok(()), Some(error)) => Err(error),
        (Ok(()), None) => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_client_frame<D: DispatchService>(
    incoming: IncomingFrame,
    dispatcher: &D,
    session: &AuthenticatedSession,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    connection: &mut ProtocolConnection,
    retained_payload: &mut BTreeMap<u64, Vec<PayloadReservation>>,
    cancelled: &mut BTreeSet<u64>,
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    unstarted: &mut VecDeque<UnstartedDispatch>,
    socket: &mut OwnedWriteHalf,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let stream_id = client_stream(&incoming.frame);
    let retains_payload = matches!(
        incoming.frame,
        ClientFrame::CallRawStart { .. } | ClientFrame::CallArgument { .. }
    );
    let dispatch_permit = if matches!(incoming.frame, ClientFrame::CallArgumentsComplete { .. }) {
        Some(resources.reserve_kernel_operation()?)
    } else {
        None
    };
    let action = version
        .receive(connection, incoming.frame)
        .map_err(|source| LocalRawSocketError::Connection { source })?;

    if retains_payload {
        retained_payload
            .entry(stream_id)
            .or_default()
            .push(incoming.reservation);
    }

    match action {
        Some(ClientAction::Dispatch { stream, call }) => {
            let StartedDispatch { accepted, future } =
                dispatcher.start(session.clone(), stream, call);
            let guards = DispatchGuards {
                _operation: dispatch_permit.expect("dispatch action requires reserved permit"),
                _payload: retained_payload.remove(&stream).unwrap_or_default(),
            };
            unstarted.push_back(UnstartedDispatch {
                stream,
                future,
                guards,
                defer_once: true,
            });
            let frame = version
                .apply(connection, accepted)
                .map_err(|source| LocalRawSocketError::Connection { source })?;
            if !write_server_frame(version, socket, &frame, shutdown).await? {
                return Ok(false);
            }
        }
        Some(ClientAction::Cancel { stream, .. }) => {
            dispatcher.cancelled(stream);
            if let Some(completion) = pending.get_mut(&stream) {
                completion.actions = VecDeque::from([completion.cancellation.clone()]);
                cancelled.remove(&stream);
            } else {
                cancelled.insert(stream);
            }
        }
        Some(ClientAction::Send(frame)) => {
            retained_payload.remove(&stream_id);
            if !write_server_frame(version, socket, &frame, shutdown).await? {
                return Ok(false);
            }
        }
        None => {}
    }
    Ok(true)
}

fn start_one_dispatch(
    unstarted: &mut VecDeque<UnstartedDispatch>,
    tasks: &mut JoinSet<(u64, DispatchCompletion)>,
) {
    let dispatch = unstarted.pop_front().expect("unstarted dispatch exists");
    tasks.spawn(async move {
        let mut completion = dispatch.future.await;
        completion._guards = Some(dispatch.guards);
        (dispatch.stream, completion)
    });
}

async fn flush_pending(
    version: &RawProtocolVersion,
    connection: &mut ProtocolConnection,
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    stream: &mut OwnedWriteHalf,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let stream_ids: Vec<_> = pending.keys().copied().collect();
    for stream_id in stream_ids {
        loop {
            let Some(action) = pending
                .get(&stream_id)
                .and_then(|completion| completion.actions.front())
                .cloned()
            else {
                pending.remove(&stream_id);
                break;
            };
            let frame = match version.apply(connection, action) {
                Ok(frame) => frame,
                Err(ConnectionError::InsufficientCredit { .. }) => break,
                Err(source) => return Err(LocalRawSocketError::Connection { source }),
            };
            if !write_server_frame(version, stream, &frame, shutdown).await? {
                return Ok(false);
            }
            pending
                .get_mut(&stream_id)
                .expect("pending completion exists")
                .actions
                .pop_front();
        }
    }
    Ok(true)
}

fn spawn_frame_reader(
    mut reader: OwnedReadHalf,
    version: RawProtocolVersion,
    resources: LocalRawSocketResources,
    sender: mpsc::Sender<Result<Option<IncomingFrame>, LocalRawSocketError>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let frame = read_versioned_client_frame(
                &mut reader,
                &version,
                &resources,
                Instant::now() + FRAME_IDLE_TIMEOUT,
            )
            .await;
            let terminal = !matches!(frame, Ok(Some(_)));
            if sender.send(frame).await.is_err() || terminal {
                return;
            }
        }
    })
}

#[cfg(test)]
async fn read_client_frame<R: AsyncRead + Unpin>(
    stream: &mut R,
    resources: &LocalRawSocketResources,
    deadline: Instant,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    read_versioned_client_frame(stream, &RawProtocolVersion::One, resources, deadline).await
}

async fn read_versioned_client_frame<R: AsyncRead + Unpin>(
    stream: &mut R,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    deadline: Instant,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    let mut header = [0_u8; FRAME_HEADER_LENGTH];
    let Some(()) = read_header_before(stream, &mut header, deadline).await? else {
        return Ok(None);
    };
    let declared = u32::from_be_bytes(header[14..18].try_into().expect("fixed header")) as usize;
    if declared > MAX_FRAME_PAYLOAD_LENGTH {
        return Err(LocalRawSocketError::Frame {
            source: FrameCodecError::PayloadTooLarge {
                actual: declared,
                maximum: MAX_FRAME_PAYLOAD_LENGTH,
            },
        });
    }
    let reservation = resources.reserve_payload(declared)?;
    let mut encoded = Vec::with_capacity(FRAME_HEADER_LENGTH + declared);
    encoded.extend_from_slice(&header);
    encoded.resize(FRAME_HEADER_LENGTH + declared, 0);
    read_exact_before(
        stream,
        &mut encoded[FRAME_HEADER_LENGTH..],
        deadline,
        LocalRawSocketError::FrameTimeout,
    )
    .await?;
    let frame = version
        .decode_client_frame(&encoded)
        .map_err(|source| LocalRawSocketError::Frame { source })?;
    Ok(Some(IncomingFrame { frame, reservation }))
}

async fn read_header_before<R: AsyncRead + Unpin>(
    stream: &mut R,
    header: &mut [u8; FRAME_HEADER_LENGTH],
    deadline: Instant,
) -> Result<Option<()>, LocalRawSocketError> {
    let mut filled = 0;
    while filled < header.len() {
        let read = timeout_at(deadline, stream.read(&mut header[filled..]))
            .await
            .map_err(|_| LocalRawSocketError::FrameTimeout)?
            .map_err(|source| LocalRawSocketError::Io { source })?;
        if read == 0 {
            if filled == 0 {
                return Ok(None);
            }
            return Err(LocalRawSocketError::Io {
                source: io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "raw frame header is truncated",
                ),
            });
        }
        filled += read;
    }
    Ok(Some(()))
}

async fn read_exact_before<R: AsyncRead + Unpin>(
    stream: &mut R,
    bytes: &mut [u8],
    deadline: Instant,
    timeout_error: LocalRawSocketError,
) -> Result<(), LocalRawSocketError> {
    timeout_at(deadline, stream.read_exact(bytes))
        .await
        .map_err(|_| timeout_error)?
        .map_err(|source| LocalRawSocketError::Io { source })?;
    Ok(())
}

async fn write_server_frame(
    version: &RawProtocolVersion,
    stream: &mut OwnedWriteHalf,
    frame: &ServerFrame,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let encoded = version
        .encode_server_frame(frame)
        .map_err(|source| LocalRawSocketError::Frame { source })?;
    write_all_until_shutdown(stream, &encoded, shutdown).await
}

async fn write_all_until_shutdown<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    bytes: &[u8],
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    if *shutdown.borrow() {
        return Ok(false);
    }
    tokio::select! {
        result = stream.write_all(bytes) => {
            result.map_err(|source| LocalRawSocketError::Io { source })?;
            Ok(true)
        }
        _ = wait_for_shutdown(shutdown) => Ok(false),
    }
}

fn report_private_dispatch_source(source: &orna_kernel_postgres::PostgresKernelError) {
    let _ = writeln!(
        io::stderr().lock(),
        "orna: protected raw client dispatch failed: {source}"
    );
}

const fn client_stream(frame: &ClientFrame) -> u64 {
    match frame {
        ClientFrame::CallRawStart { stream, .. }
        | ClientFrame::CallArgument { stream, .. }
        | ClientFrame::CallArgumentsComplete { stream }
        | ClientFrame::WindowUpdate { stream, .. }
        | ClientFrame::CallCancel { stream } => *stream,
        ClientFrame::Ping { .. } => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        future::poll_fn,
        os::unix::net::UnixStream as BlockingUnixStream,
        path::PathBuf,
        str::FromStr,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        task::Poll,
    };

    use orna_core::{
        CatalogueRevisionId, FunctionId, InvocationId, PrincipalId, SchemaId, SourceRevisionId,
        TypeId,
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, SchemaDefinition,
        },
        revision::RevisionPair,
        security::{
            AuthenticatedSession, Principal, PrincipalKind, PrincipalStatus, SecuritySnapshot,
        },
        value::{EnumValue, RuntimeValue},
    };
    use orna_protocol::{
        Channel, ClientFrame, Event, ServerFrame, decode_catalogue_server_frame,
        decode_server_frame, encode_catalogue_client_frame, encode_client_frame,
    };
    use tokio::sync::Notify;
    use tokio::time::timeout;

    use super::*;

    const FUNCTION: FunctionId = FunctionId::from_bytes([1; 16]);
    const ENUM_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);

    fn enum_catalogue() -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x32; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x33; 16]),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                ENUM_TYPE,
                QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                ["lead", "qualified"],
            )],
            vec![],
        )
        .unwrap()
    }

    #[derive(Clone)]
    struct TestDispatch {
        actions: Arc<Vec<ServerAction>>,
        cancelled: Arc<AtomicBool>,
        polled: Arc<AtomicBool>,
        first_poll_saw_cancellation: Arc<AtomicBool>,
    }

    impl TestDispatch {
        fn new(actions: Vec<ServerAction>) -> Self {
            Self {
                actions: Arc::new(actions),
                cancelled: Arc::new(AtomicBool::new(false)),
                polled: Arc::new(AtomicBool::new(false)),
                first_poll_saw_cancellation: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl DispatchService for TestDispatch {
        fn start(
            &self,
            _session: AuthenticatedSession,
            stream: u64,
            _call: RawCall,
        ) -> StartedDispatch {
            let cancelled = Arc::clone(&self.cancelled);
            let polled = Arc::clone(&self.polled);
            let first_poll_saw_cancellation = Arc::clone(&self.first_poll_saw_cancellation);
            let actions = Arc::clone(&self.actions);
            let future = Box::pin(async move {
                poll_fn(move |_| {
                    polled.store(true, Ordering::SeqCst);
                    first_poll_saw_cancellation
                        .store(cancelled.load(Ordering::SeqCst), Ordering::SeqCst);
                    Poll::Ready(())
                })
                .await;
                DispatchCompletion {
                    actions: actions.iter().cloned().collect(),
                    cancellation: ServerAction::Cancelled { stream },
                    _guards: None,
                }
            });
            StartedDispatch {
                accepted: ServerAction::Accepted {
                    stream,
                    invocation: InvocationId::from_bytes([9; 16]),
                },
                future,
            }
        }

        fn cancelled(&self, _stream: u64) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }

    #[derive(Clone)]
    struct GatedDispatch {
        release: Arc<Notify>,
        polled: Arc<AtomicBool>,
    }

    impl GatedDispatch {
        fn new() -> Self {
            Self {
                release: Arc::new(Notify::new()),
                polled: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl DispatchService for GatedDispatch {
        fn start(
            &self,
            _session: AuthenticatedSession,
            stream: u64,
            _call: RawCall,
        ) -> StartedDispatch {
            let release = Arc::clone(&self.release);
            let polled = Arc::clone(&self.polled);
            StartedDispatch {
                accepted: ServerAction::Accepted {
                    stream,
                    invocation: InvocationId::from_bytes([9; 16]),
                },
                future: Box::pin(async move {
                    polled.store(true, Ordering::SeqCst);
                    release.notified().await;
                    DispatchCompletion {
                        actions: VecDeque::from([ServerAction::Completed { stream }]),
                        cancellation: ServerAction::Cancelled { stream },
                        _guards: None,
                    }
                }),
            }
        }
    }

    #[test]
    fn handshake_bytes_and_listener_budgets_are_exact() {
        assert_eq!(CLIENT_HELLO, *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00");
        assert_eq!(
            CLIENT_CATALOGUE_HELLO,
            *b"ORNA\x01\x00\x00\x02\x00\x00\x00\x00"
        );
        assert_eq!(
            CLIENT_ACTIVE_HELLO,
            *b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00"
        );
        assert_eq!(
            CLIENT_REGISTERED_HELLO,
            *b"ORNA\x01\x00\x00\x04\x00\x00\x00\x00"
        );
        assert_eq!(SERVER_ACK, *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00");
        assert_eq!(
            SERVER_CATALOGUE_ACK,
            *b"ORNA\x81\x00\x00\x02\x00\x00\x00\x00"
        );
        assert_eq!(SERVER_ACTIVE_ACK, *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00");
        assert_eq!(
            SERVER_REGISTERED_ACK,
            *b"ORNA\x81\x00\x00\x04\x00\x00\x00\x00"
        );
        assert_eq!(
            requested_protocol(&CLIENT_ACTIVE_HELLO),
            Some(RequestedProtocol::Active)
        );
        assert_eq!(
            requested_protocol(&CLIENT_REGISTERED_HELLO),
            Some(RequestedProtocol::Registered)
        );
        assert_eq!(
            requested_protocol(b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00"),
            None
        );

        let resources = LocalRawSocketResources::new();
        let payload = resources
            .reserve_payload(SHARED_PAYLOAD_BYTES)
            .expect("complete payload budget");
        assert!(matches!(
            resources.reserve_payload(1),
            Err(LocalRawSocketError::PayloadCapacity)
        ));
        drop(payload);
        assert!(resources.reserve_payload(1).is_ok());

        let operations: Vec<_> = (0..KERNEL_OPERATION_LIMIT)
            .map(|_| {
                resources
                    .reserve_kernel_operation()
                    .expect("operation permit")
            })
            .collect();
        assert!(matches!(
            resources.reserve_kernel_operation(),
            Err(LocalRawSocketError::KernelCapacity)
        ));
        drop(operations);
        assert!(resources.reserve_kernel_operation().is_ok());
    }

    #[tokio::test]
    async fn catalogue_connection_drives_enum_arguments_and_results() {
        let catalogue = Arc::new(enum_catalogue());
        let value = RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap());
        let dispatcher = TestDispatch::new(vec![
            ServerAction::Events {
                stream: 1,
                events: vec![Event::Value(value.clone())],
            },
            ServerAction::Completed { stream: 1 },
        ]);
        let resources = LocalRawSocketResources::new();
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
            dispatcher,
            test_session(),
            RawProtocolVersion::Catalogue(Arc::clone(&catalogue)),
            server,
            resources,
            watch::channel(false).1,
        ));

        for frame in [
            ClientFrame::CallRawStart {
                stream: 1,
                function: FUNCTION,
            },
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 1024,
            },
            ClientFrame::CallArgument {
                stream: 1,
                parameter: orna_core::ParameterId::from_bytes([0x34; 16]),
                value: value.clone(),
            },
            ClientFrame::CallArgumentsComplete { stream: 1 },
        ] {
            client
                .write_all(
                    &encode_catalogue_client_frame(&catalogue, &frame)
                        .expect("catalogue client frame encodes"),
                )
                .await
                .expect("catalogue client frame writes");
        }

        assert!(matches!(
            read_catalogue_server_frame(&mut client, &catalogue).await,
            ServerFrame::CallAccepted { stream: 1, .. }
        ));
        assert!(matches!(
            read_catalogue_server_frame(&mut client, &catalogue).await,
            ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultValues,
                events,
            } if events.len() == 1 && events[0].event == Event::Value(value)
        ));
        assert_eq!(
            read_catalogue_server_frame(&mut client, &catalogue).await,
            ServerFrame::CallCompleted { stream: 1 }
        );

        client.shutdown().await.expect("client shutdown");
        server_task
            .await
            .expect("catalogue connection task")
            .expect("catalogue connection closes");
    }

    #[tokio::test]
    async fn fragmented_frames_flow_control_and_eof_preserve_bounded_state() {
        let dispatcher = TestDispatch::new(vec![
            ServerAction::Events {
                stream: 1,
                events: vec![Event::Value(RuntimeValue::Boolean(true))],
            },
            ServerAction::Completed { stream: 1 },
        ]);
        let resources = LocalRawSocketResources::new();
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let server_task = tokio::spawn(drive_authenticated_stream(
            dispatcher,
            test_session(),
            server,
            resources.clone(),
        ));

        let ping =
            encode_client_frame(&ClientFrame::Ping { token: [7; 8] }).expect("PING frame encodes");
        for byte in ping {
            client.write_all(&[byte]).await.expect("fragment writes");
        }
        assert_eq!(
            read_server_frame(&mut client).await,
            ServerFrame::Pong { token: [7; 8] }
        );

        send_client_frame(
            &mut client,
            &ClientFrame::CallRawStart {
                stream: 1,
                function: FUNCTION,
            },
        )
        .await;
        send_client_frame(
            &mut client,
            &ClientFrame::CallArgumentsComplete { stream: 1 },
        )
        .await;
        assert!(matches!(
            read_server_frame(&mut client).await,
            ServerFrame::CallAccepted { stream: 1, .. }
        ));
        assert!(
            timeout(Duration::from_millis(25), read_server_frame(&mut client))
                .await
                .is_err()
        );

        let ping =
            encode_client_frame(&ClientFrame::Ping { token: [8; 8] }).expect("PING frame encodes");
        for byte in ping {
            client
                .write_all(&[byte])
                .await
                .expect("concurrent fragment writes");
        }
        assert_eq!(
            read_server_frame(&mut client).await,
            ServerFrame::Pong { token: [8; 8] }
        );

        send_client_frame(
            &mut client,
            &ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 1024,
            },
        )
        .await;
        assert!(matches!(
            read_server_frame(&mut client).await,
            ServerFrame::EventBatch { stream: 1, .. }
        ));
        assert_eq!(
            read_server_frame(&mut client).await,
            ServerFrame::CallCompleted { stream: 1 }
        );

        client.shutdown().await.expect("client shutdown");
        server_task
            .await
            .expect("connection task")
            .expect("clean EOF");
        assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT
        );
    }

    #[tokio::test]
    async fn buffered_cancel_precedes_first_finish_poll_but_finish_still_runs() {
        let dispatcher = TestDispatch::new(vec![ServerAction::Completed { stream: 1 }]);
        let resources = LocalRawSocketResources::new();
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let server_task = tokio::spawn(drive_authenticated_stream(
            dispatcher.clone(),
            test_session(),
            server,
            resources,
        ));

        let frames = [
            ClientFrame::CallRawStart {
                stream: 1,
                function: FUNCTION,
            },
            ClientFrame::CallArgumentsComplete { stream: 1 },
            ClientFrame::CallCancel { stream: 1 },
        ];
        let encoded: Vec<_> = frames
            .iter()
            .flat_map(|frame| encode_client_frame(frame).expect("client frame encodes"))
            .collect();
        client.write_all(&encoded).await.expect("buffered frames");

        assert!(matches!(
            read_server_frame(&mut client).await,
            ServerFrame::CallAccepted { stream: 1, .. }
        ));
        assert_eq!(
            read_server_frame(&mut client).await,
            ServerFrame::CallCancelled { stream: 1 }
        );
        assert!(
            dispatcher
                .first_poll_saw_cancellation
                .load(Ordering::SeqCst)
        );

        client.shutdown().await.expect("client shutdown");
        server_task
            .await
            .expect("connection task")
            .expect("clean EOF");
    }

    #[tokio::test]
    async fn peer_failure_after_acceptance_still_polls_protected_work() {
        let dispatcher = TestDispatch::new(vec![ServerAction::Completed { stream: 1 }]);
        let polled = Arc::clone(&dispatcher.polled);
        let resources = LocalRawSocketResources::new();
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let server_task = tokio::spawn(drive_authenticated_stream(
            dispatcher,
            test_session(),
            server,
            resources,
        ));
        for frame in [
            ClientFrame::CallRawStart {
                stream: 1,
                function: FUNCTION,
            },
            ClientFrame::CallArgumentsComplete { stream: 1 },
        ] {
            client
                .write_all(&encode_client_frame(&frame).expect("client frame encodes"))
                .await
                .expect("client frame writes");
        }
        drop(client);

        let _ = timeout(Duration::from_secs(1), server_task)
            .await
            .expect("connection drains after peer failure")
            .expect("connection task joins");
        assert!(polled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn server_shutdown_stops_reads_but_drains_accepted_work() {
        let dispatcher = GatedDispatch::new();
        let polled = Arc::clone(&dispatcher.polled);
        let resources = LocalRawSocketResources::new();
        let (shutdown_sender, shutdown) = watch::channel(false);
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let mut server_task = tokio::spawn(drive_authenticated_stream_until_shutdown(
            dispatcher.clone(),
            test_session(),
            server,
            resources,
            shutdown,
        ));

        shutdown_sender.send(false).expect("false signal");
        send_parameter_free_call(&mut client, 1).await;
        assert!(matches!(
            read_server_frame(&mut client).await,
            ServerFrame::CallAccepted { stream: 1, .. }
        ));
        shutdown_sender.send(true).expect("shutdown signal");
        assert!(
            timeout(Duration::from_millis(25), &mut server_task)
                .await
                .is_err(),
            "shutdown returned before protected work drained"
        );
        assert!(polled.load(Ordering::SeqCst));

        dispatcher.release.notify_one();
        server_task
            .await
            .expect("connection task")
            .expect("ordered shutdown");
        let mut byte = [0_u8; 1];
        assert_eq!(client.read(&mut byte).await.expect("shutdown EOF"), 0);
    }

    #[tokio::test]
    async fn server_shutdown_interrupts_a_blocked_socket_write() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let (shutdown_sender, mut shutdown) = watch::channel(false);
        let mut write = tokio::spawn(async move {
            write_all_until_shutdown(&mut writer, &[1, 2], &mut shutdown).await
        });
        tokio::task::yield_now().await;

        shutdown_sender.send(false).expect("false signal");
        assert!(
            timeout(Duration::from_millis(25), &mut write)
                .await
                .is_err(),
            "a false value incorrectly signalled shutdown"
        );

        shutdown_sender.send(true).expect("shutdown signal");
        assert!(!write.await.expect("write task").expect("shutdown write"));
    }

    #[test]
    fn fixed_listener_replaces_only_a_verified_stale_socket_and_drains_connections() {
        let runtime_directory = listener_test_directory();
        let _ = fs::remove_dir_all(&runtime_directory);
        fs::create_dir_all(&runtime_directory).expect("runtime directory");
        fs::set_permissions(&runtime_directory, fs::Permissions::from_mode(0o711))
            .expect("runtime directory mode");
        let socket_path = runtime_directory.join(SOCKET_NAME);
        let stale = StandardUnixListener::bind(&socket_path).expect("stale socket");
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o666))
            .expect("stale socket mode");
        drop(stale);

        let server = start_local_raw_socket(&runtime_directory, unavailable_kernel())
            .expect("fixed listener starts");
        assert!(server.is_healthy());
        let metadata = fs::symlink_metadata(&socket_path).expect("public socket metadata");
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.mode() & 0o7777, 0o666);
        assert_eq!(metadata.nlink(), 1);

        let clients: Vec<_> = (0..CONNECTION_LIMIT)
            .map(|_| BlockingUnixStream::connect(&socket_path).expect("admitted connection"))
            .collect();
        std::thread::sleep(Duration::from_millis(25));
        let mut rejected = BlockingUnixStream::connect(&socket_path).expect("capacity connection");
        rejected
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("capacity timeout");
        let mut byte = [0_u8; 1];
        assert_eq!(
            std::io::Read::read(&mut rejected, &mut byte).expect("capacity close"),
            0
        );

        drop(clients);
        server.stop().expect("ordered listener stop");
        assert!(!socket_path.exists());

        fs::write(&socket_path, b"hostile").expect("hostile socket path");
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o666))
            .expect("hostile path mode");
        assert!(matches!(
            start_local_raw_socket(&runtime_directory, unavailable_kernel()),
            Err(LocalRawSocketServerError::InvalidSocketState)
        ));
        fs::remove_file(&socket_path).expect("hostile path cleanup");
        fs::remove_dir(&runtime_directory).expect("runtime directory cleanup");
    }

    fn listener_test_directory() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!("raw-socket-listener-{}", std::process::id()))
    }

    #[tokio::test]
    async fn shared_dispatch_limit_rejects_a_second_connection_before_acceptance() {
        let resources = LocalRawSocketResources::new();
        let held: Vec<_> = (1..KERNEL_OPERATION_LIMIT)
            .map(|_| {
                resources
                    .reserve_kernel_operation()
                    .expect("operation permit")
            })
            .collect();
        let gated = GatedDispatch::new();
        let (first_server, mut first_client) = UnixStream::pair().expect("first stream pair");
        let first_task = tokio::spawn(drive_authenticated_stream(
            gated.clone(),
            test_session(),
            first_server,
            resources.clone(),
        ));
        send_parameter_free_call(&mut first_client, 1).await;
        assert!(matches!(
            read_server_frame(&mut first_client).await,
            ServerFrame::CallAccepted { stream: 1, .. }
        ));
        assert_eq!(resources.kernel_operations.available_permits(), 0);

        let (second_server, mut second_client) = UnixStream::pair().expect("second stream pair");
        let second_task = tokio::spawn(drive_authenticated_stream(
            TestDispatch::new(vec![ServerAction::Completed { stream: 1 }]),
            test_session(),
            second_server,
            resources.clone(),
        ));
        send_parameter_free_call(&mut second_client, 1).await;
        let mut response = [0_u8; 1];
        assert_eq!(
            second_client
                .read(&mut response)
                .await
                .expect("capacity close"),
            0
        );
        assert!(matches!(
            second_task.await.expect("second connection task"),
            Err(LocalRawSocketError::KernelCapacity)
        ));
        assert_eq!(resources.kernel_operations.available_permits(), 0);

        gated.release.notify_one();
        assert_eq!(
            read_server_frame(&mut first_client).await,
            ServerFrame::CallCompleted { stream: 1 }
        );
        first_client
            .shutdown()
            .await
            .expect("first client shutdown");
        first_task
            .await
            .expect("first connection task")
            .expect("first connection closes");
        drop(held);
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT
        );
    }

    #[tokio::test]
    async fn retained_payload_and_operation_guards_survive_cancelled_connection_drain() {
        let resources = LocalRawSocketResources::new();
        let gated = GatedDispatch::new();
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let server_task = tokio::spawn(drive_authenticated_stream(
            gated.clone(),
            test_session(),
            server,
            resources.clone(),
        ));
        let start = ClientFrame::CallRawStart {
            stream: 1,
            function: FUNCTION,
        };
        let complete = ClientFrame::CallArgumentsComplete { stream: 1 };
        let retained = encode_client_frame(&start)
            .expect("start frame encodes")
            .len()
            - FRAME_HEADER_LENGTH;
        send_client_frame(&mut client, &start).await;
        send_client_frame(&mut client, &complete).await;
        assert!(matches!(
            read_server_frame(&mut client).await,
            ServerFrame::CallAccepted { stream: 1, .. }
        ));
        send_client_frame(&mut client, &ClientFrame::CallCancel { stream: 1 }).await;
        drop(client);
        while !gated.polled.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            resources.payload.available_permits(),
            SHARED_PAYLOAD_BYTES - retained
        );
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT - 1
        );

        gated.release.notify_one();
        let _ = server_task
            .await
            .expect("connection task drains after cancellation");
        assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT
        );
    }

    #[tokio::test]
    async fn completed_dispatch_retains_guards_until_flow_control_delivers_it() {
        let resources = LocalRawSocketResources::new();
        let dispatcher = TestDispatch::new(vec![
            ServerAction::Events {
                stream: 1,
                events: vec![Event::Value(RuntimeValue::Boolean(true))],
            },
            ServerAction::Completed { stream: 1 },
        ]);
        let witness = dispatcher.clone();
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let server_task = tokio::spawn(drive_authenticated_stream(
            dispatcher,
            test_session(),
            server,
            resources.clone(),
        ));
        send_parameter_free_call(&mut client, 1).await;
        assert!(matches!(
            read_server_frame(&mut client).await,
            ServerFrame::CallAccepted { stream: 1, .. }
        ));
        while !witness.polled.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT - 1
        );
        assert!(resources.payload.available_permits() < SHARED_PAYLOAD_BYTES);

        send_client_frame(
            &mut client,
            &ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 1024,
            },
        )
        .await;
        assert!(matches!(
            read_server_frame(&mut client).await,
            ServerFrame::EventBatch { stream: 1, .. }
        ));
        assert_eq!(
            read_server_frame(&mut client).await,
            ServerFrame::CallCompleted { stream: 1 }
        );
        for _ in 0..16 {
            if resources.kernel_operations.available_permits() == KERNEL_OPERATION_LIMIT {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT
        );
        assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);

        client.shutdown().await.expect("client shutdown");
        server_task
            .await
            .expect("connection task")
            .expect("connection closes");
    }

    #[tokio::test]
    async fn aborting_the_public_waiter_does_not_cancel_owned_connection_work() {
        let resources = LocalRawSocketResources::new();
        let gated = GatedDispatch::new();
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let completed = Arc::new(Notify::new());
        let completion_witness = Arc::clone(&completed);
        let owned_resources = resources.clone();
        let owned_dispatch = gated.clone();
        let (shutdown_guard, _shutdown) = watch::channel(false);
        let owned = run_owned_connection_with_shutdown_guard(shutdown_guard, async move {
            let result =
                drive_authenticated_stream(owned_dispatch, test_session(), server, owned_resources)
                    .await;
            completion_witness.notify_one();
            result
        });
        let waiter = tokio::spawn(owned);
        send_parameter_free_call(&mut client, 1).await;
        assert!(matches!(
            read_server_frame(&mut client).await,
            ServerFrame::CallAccepted { stream: 1, .. }
        ));
        waiter.abort();
        assert!(
            waiter
                .await
                .expect_err("waiter is cancelled")
                .is_cancelled()
        );
        while !gated.polled.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT - 1
        );

        drop(client);
        gated.release.notify_one();
        timeout(Duration::from_secs(1), completed.notified())
            .await
            .expect("detached connection and reader terminate");
    }

    #[tokio::test]
    async fn invalid_hello_and_exhausted_authentication_capacity_close_silently() {
        let resources = LocalRawSocketResources::new();
        let (server, client) = StandardUnixStream::pair().expect("Unix stream pair");
        client.set_nonblocking(true).expect("nonblocking client");
        let mut client = UnixStream::from_std(client).expect("Tokio client stream");
        let invalid_task = tokio::spawn(serve_local_raw_stream(
            unavailable_kernel(),
            server,
            resources.clone(),
        ));
        let mut invalid = CLIENT_HELLO;
        invalid[4] = 0x02;
        client
            .write_all(&invalid)
            .await
            .expect("invalid hello writes");
        let mut response = [0_u8; 1];
        assert_eq!(client.read(&mut response).await.expect("silent close"), 0);
        assert!(matches!(
            invalid_task.await.expect("invalid hello task"),
            Err(LocalRawSocketError::InvalidHello)
        ));

        let operation_permits: Vec<_> = (0..KERNEL_OPERATION_LIMIT)
            .map(|_| {
                resources
                    .reserve_kernel_operation()
                    .expect("operation permit")
            })
            .collect();
        let (server, client) = StandardUnixStream::pair().expect("Unix stream pair");
        client.set_nonblocking(true).expect("nonblocking client");
        let mut client = UnixStream::from_std(client).expect("Tokio client stream");
        let capacity_task = tokio::spawn(serve_local_raw_stream(
            unavailable_kernel(),
            server,
            resources,
        ));
        client
            .write_all(&CLIENT_HELLO)
            .await
            .expect("valid hello writes");
        assert_eq!(client.read(&mut response).await.expect("silent close"), 0);
        assert!(matches!(
            capacity_task.await.expect("capacity task"),
            Err(LocalRawSocketError::KernelCapacity)
        ));
        drop(operation_permits);
    }

    #[tokio::test]
    async fn oversized_and_unfunded_payloads_fail_before_payload_reads() {
        let resources = LocalRawSocketResources::new();
        let (mut server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let mut oversized = [0_u8; FRAME_HEADER_LENGTH];
        oversized[..4].copy_from_slice(b"ORF1");
        oversized[4] = 0x06;
        oversized[14..].copy_from_slice(&((MAX_FRAME_PAYLOAD_LENGTH + 1) as u32).to_be_bytes());
        client
            .write_all(&oversized)
            .await
            .expect("oversized header writes");
        assert!(matches!(
            read_client_frame(
                &mut server,
                &resources,
                Instant::now() + Duration::from_secs(1)
            )
            .await,
            Err(LocalRawSocketError::Frame {
                source: FrameCodecError::PayloadTooLarge { .. }
            })
        ));
        assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);

        let payload = resources
            .reserve_payload(SHARED_PAYLOAD_BYTES)
            .expect("complete payload budget");
        let (mut server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let mut unfunded = [0_u8; FRAME_HEADER_LENGTH];
        unfunded[..4].copy_from_slice(b"ORF1");
        unfunded[4] = 0x06;
        unfunded[14..].copy_from_slice(&1_u32.to_be_bytes());
        client
            .write_all(&unfunded)
            .await
            .expect("unfunded header writes");
        assert!(matches!(
            timeout(
                Duration::from_millis(25),
                read_client_frame(
                    &mut server,
                    &resources,
                    Instant::now() + Duration::from_secs(1)
                )
            )
            .await
            .expect("capacity rejects before payload read"),
            Err(LocalRawSocketError::PayloadCapacity)
        ));
        drop(payload);
    }

    fn test_session() -> AuthenticatedSession {
        let principal = PrincipalId::from_bytes([4; 16]);
        SecuritySnapshot::new(
            RevisionPair::new(
                SourceRevisionId::from_bytes([2; 16]),
                CatalogueRevisionId::from_bytes([3; 16]),
            ),
            vec![FUNCTION],
            vec![Principal::new(
                principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )
        .expect("security snapshot")
        .bind_authenticated_session(principal, vec![])
        .expect("authenticated session")
    }

    fn unavailable_kernel() -> PostgresKernel {
        PostgresKernel::from_str("host=127.0.0.1 port=1 dbname=absent")
            .expect("configuration parses without connecting")
    }

    async fn send_client_frame(stream: &mut UnixStream, frame: &ClientFrame) {
        stream
            .write_all(&encode_client_frame(frame).expect("client frame encodes"))
            .await
            .expect("client frame writes");
    }

    async fn send_parameter_free_call(stream: &mut UnixStream, stream_id: u64) {
        send_client_frame(
            stream,
            &ClientFrame::CallRawStart {
                stream: stream_id,
                function: FUNCTION,
            },
        )
        .await;
        send_client_frame(
            stream,
            &ClientFrame::CallArgumentsComplete { stream: stream_id },
        )
        .await;
    }

    async fn read_server_frame(stream: &mut UnixStream) -> ServerFrame {
        let encoded = read_encoded_server_frame(stream, "server frame").await;
        decode_server_frame(&encoded).expect("server frame decodes")
    }

    async fn read_catalogue_server_frame(
        stream: &mut UnixStream,
        catalogue: &CatalogueSnapshot,
    ) -> ServerFrame {
        let encoded = read_encoded_server_frame(stream, "catalogue server frame").await;
        decode_catalogue_server_frame(catalogue, &encoded).expect("catalogue server frame decodes")
    }

    async fn read_encoded_server_frame(stream: &mut UnixStream, name: &str) -> Vec<u8> {
        let mut header = [0_u8; FRAME_HEADER_LENGTH];
        timeout(Duration::from_secs(1), stream.read_exact(&mut header))
            .await
            .unwrap_or_else(|_| panic!("{name} timeout"))
            .unwrap_or_else(|error| panic!("{name} header: {error}"));
        let length = u32::from_be_bytes(header[14..18].try_into().expect("fixed header")) as usize;
        let mut encoded = header.to_vec();
        encoded.resize(FRAME_HEADER_LENGTH + length, 0);
        stream
            .read_exact(&mut encoded[FRAME_HEADER_LENGTH..])
            .await
            .unwrap_or_else(|error| panic!("{name} payload: {error}"));
        encoded
    }
}
