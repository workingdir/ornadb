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
    value::{OpaqueCodecRegistry, RuntimeValue},
};
use orna_postgres::{PostgresKernel, PostgresKernelError, ResourceCancellation};
use orna_protocol::{
    CallFailure, ClientAction, ClientFrame, ConnectionError, FrameCodecError,
    MAX_FRAME_PAYLOAD_LENGTH, ProtocolConnection, RawCall, ResourceClientFrame,
    ResourceConnectionError, ResourceFrameDisposition, ResourceProtocolConnection, ResourceRequest,
    ResourceServerFrame, ResourceKind, ServerAction, ServerFrame, decode_active_client_frame,
    decode_catalogue_client_frame, decode_client_frame, decode_constructed_client_frame,
    decode_registered_client_frame, decode_resource_client_frame, encode_active_server_frame,
    encode_catalogue_server_frame, encode_constructed_server_frame, encode_constructed_value,
    encode_registered_server_frame,
    encode_resource_server_frame, encode_server_frame,
};
use orna_standard::{RegisteredOpaqueCodecsError, registered_opaque_codecs};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::{JoinError, JoinHandle, JoinSet},
    time::{Instant, timeout_at},
};

use crate::{RawClientDispatch, authenticate_local_stream};

const CLIENT_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00";
const CLIENT_CATALOGUE_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x02\x00\x00\x00\x00";
const CLIENT_ACTIVE_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00";
const CLIENT_REGISTERED_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x04\x00\x00\x00\x00";
const CLIENT_CONSTRUCTED_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00";
const SERVER_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00";
const SERVER_CATALOGUE_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x02\x00\x00\x00\x00";
const SERVER_ACTIVE_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00";
const SERVER_REGISTERED_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x04\x00\x00\x00\x00";
const SERVER_CONSTRUCTED_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00";
const FRAME_HEADER_LENGTH: usize = 18;
const RESOURCE_MARKER: &[u8; 4] = b"ORNA";
const RESOURCE_HEADER_LENGTH: usize = 21;
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
    Constructed(Arc<ActiveDatabaseRevision>, Arc<OpaqueCodecRegistry>),
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
            Self::Constructed(active, registry) => {
                decode_constructed_client_frame(active, registry, encoded)
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
            Self::Constructed(active, registry) => {
                encode_constructed_server_frame(active, registry, frame)
            }
        }
    }

    fn decode_resource_client_frame(
        &self,
        encoded: &[u8],
    ) -> Result<ResourceClientFrame, FrameCodecError> {
        match self {
            Self::Constructed(active, registry) => {
                decode_resource_client_frame(active, registry, encoded)
            }
            Self::One | Self::Catalogue(_) | Self::Active(_) | Self::Registered(_, _) => {
                Err(FrameCodecError::ResourceRequiresConstructed)
            }
        }
    }

    fn encode_resource_server_frame(
        &self,
        frame: &ResourceServerFrame,
    ) -> Result<Vec<u8>, FrameCodecError> {
        match self {
            Self::Constructed(active, registry) => {
                encode_resource_server_frame(active, registry, frame)
            }
            Self::One | Self::Catalogue(_) | Self::Active(_) | Self::Registered(_, _) => {
                Err(FrameCodecError::ResourceRequiresConstructed)
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
            Self::Constructed(active, registry) => {
                connection.receive_constructed(active, registry, frame)
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
            Self::Constructed(active, registry) => {
                connection.apply_constructed(active, registry, action)
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
    Constructed,
}

impl RequestedProtocol {
    const fn acknowledgement(self) -> &'static [u8; 12] {
        match self {
            Self::One => &SERVER_ACK,
            Self::Catalogue => &SERVER_CATALOGUE_ACK,
            Self::Active => &SERVER_ACTIVE_ACK,
            Self::Registered => &SERVER_REGISTERED_ACK,
            Self::Constructed => &SERVER_CONSTRUCTED_ACK,
        }
    }
}

fn requested_protocol(hello: &[u8; 12]) -> Option<RequestedProtocol> {
    match *hello {
        CLIENT_HELLO => Some(RequestedProtocol::One),
        CLIENT_CATALOGUE_HELLO => Some(RequestedProtocol::Catalogue),
        CLIENT_ACTIVE_HELLO => Some(RequestedProtocol::Active),
        CLIENT_REGISTERED_HELLO => Some(RequestedProtocol::Registered),
        CLIENT_CONSTRUCTED_HELLO => Some(RequestedProtocol::Constructed),
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
    async fn acquire_kernel_operation(
        &self,
        cancellation: &ResourceCancellation,
    ) -> Option<OwnedSemaphorePermit> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            permit = Arc::clone(&self.kernel_operations).acquire_owned() => Some(
                permit.expect("local raw socket kernel semaphore remains open")
            ),
        }
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
    /// A decoded resource frame violated the resource state machine.
    ResourceConnection {
        /// The resource state-machine failure.
        source: ResourceConnectionError,
    },
    /// Recording a resource cancellation audit failed.
    ResourceCancellationAudit {
        /// The protected cancellation-audit failure.
        source: Box<PostgresKernelError>,
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
            Self::Frame { .. } => "local raw socket frame is invalid",
            Self::Io { .. } => "local raw socket I/O failed",
            Self::Connection { .. } => "local raw socket state is invalid",
            Self::ResourceConnection { .. } => "local raw socket resource state is invalid",
            Self::ResourceCancellationAudit { .. } => {
                "local raw socket resource cancellation audit failed"
            }
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
            Self::ResourceConnection { source } => Some(source),
            Self::ResourceCancellationAudit { source } => Some(source),
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
                let (active, registry) = recover_active_and_registry(&kernel).await?;
                RawProtocolVersion::Registered(active, registry)
            }
            RequestedProtocol::Constructed => {
                let (active, registry) = recover_active_and_registry(&kernel).await?;
                RawProtocolVersion::Constructed(active, registry)
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

async fn recover_active_and_registry(
    kernel: &PostgresKernel,
) -> Result<(Arc<ActiveDatabaseRevision>, Arc<OpaqueCodecRegistry>), LocalRawSocketError> {
    let active =
        Arc::new(
            kernel
                .recover()
                .await
                .map_err(|source| LocalRawSocketError::ActiveRevision {
                    source: Box::new(source),
                })?,
        );
    let standard =
        active
            .catalogue_hash_context()
            .standard()
            .ok_or(LocalRawSocketError::OpaqueRegistry {
                source: RegisteredOpaqueCodecsError::UnacceptedStandardSnapshot,
            })?;
    let registry = registered_opaque_codecs(standard)
        .map_err(|source| LocalRawSocketError::OpaqueRegistry { source })?;
    Ok((active, Arc::new(registry)))
}

struct PayloadReservation {
    _permit: Option<OwnedSemaphorePermit>,
}

struct DispatchGuards {
    _operation: OwnedSemaphorePermit,
    _payload: Vec<PayloadReservation>,
}

struct RawIncomingFrame {
    frame: ClientFrame,
    reservation: PayloadReservation,
}

enum IncomingFrame {
    Raw(RawIncomingFrame),
    Resource {
        frame: ResourceClientFrame,
        reservation: PayloadReservation,
    },
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

struct ResourceDispatchCompletion {
    actions: VecDeque<ResourceServerFrame>,
}

struct StartedResourceDispatch {
    future: ResourceDispatchFuture,
    cancellation: ResourceCancellation,
}

struct ResourceTask {
    handle: JoinHandle<()>,
    cancellation: ResourceCancellation,
}

type DispatchFuture = Pin<Box<dyn Future<Output = DispatchCompletion> + Send>>;
type ResourceDispatchFuture = Pin<Box<dyn Future<Output = ResourceDispatchCompletion> + Send>>;

#[derive(Clone)]
struct RawDispatchService {
    kernel: PostgresKernel,
}

trait DispatchService: Clone + Send + Sync + 'static {
    fn start(&self, session: AuthenticatedSession, stream: u64, call: RawCall) -> StartedDispatch;

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        _request: ResourceRequest,
        _resources: LocalRawSocketResources,
        _version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        None
    }

    fn cancelled(&self, _stream: u64) {}
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

    fn start_resource(
        &self,
        session: AuthenticatedSession,
        request: ResourceRequest,
        resources: LocalRawSocketResources,
        version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        let kernel = self.kernel.clone();
        let cancellation = ResourceCancellation::new();
        let operation_cancellation = cancellation.clone();
        let future = Box::pin(async move {
            let Some(_operation) = resources
                .acquire_kernel_operation(&operation_cancellation)
                .await
            else {
                return ResourceDispatchCompletion {
                    actions: VecDeque::new(),
                };
            };
            let result = kernel
                .dispatch_authenticated_server_resource_with_cancellation(
                    &session,
                    &request,
                    &operation_cancellation,
                )
                .await;
            let actions = match result {
                Ok(Some(result)) => resource_completion_actions(&version, &request, Ok(result)),
                Ok(None) => VecDeque::new(),
                Err(error) => resource_completion_actions(&version, &request, Err(error)),
            };
            ResourceDispatchCompletion { actions }
        });
        Some(StartedResourceDispatch {
            future,
            cancellation,
        })
    }
}

fn resource_completion_actions(
    version: &RawProtocolVersion,
    request: &ResourceRequest,
    result: Result<orna_postgres::AuthenticatedServerResourceResult, PostgresKernelError>,
) -> VecDeque<ResourceServerFrame> {
    match result {
        Ok(orna_postgres::AuthenticatedServerResourceResult::Failed {
            stream_id,
            request_id,
            failure,
        }) => VecDeque::from([ResourceServerFrame::Failed(orna_protocol::ResourceFailed {
            stream_id,
            request_id,
            failure,
        })]),
        Ok(orna_postgres::AuthenticatedServerResourceResult::Completed {
            stream_id,
            request_id,
            nested_invocation_id,
            target_revision,
            resource_kind,
            values,
        }) => {
            let mut actions = VecDeque::new();
            actions.push_back(ResourceServerFrame::Accepted(
                orna_protocol::ResourceAccepted {
                    stream_id,
                    request_id,
                    nested_invocation_id,
                    target_revision,
                    resource_kind,
                },
            ));
            for (batch_sequence, value) in values.into_iter().enumerate() {
                let Ok(batch_sequence) = u64::try_from(batch_sequence) else {
                    return resource_internal_failure(request);
                };
                actions.push_back(ResourceServerFrame::Values(orna_protocol::ResourceValues {
                    stream_id,
                    request_id,
                    batch_sequence,
                    item_count: 1,
                    byte_count: match resource_value_byte_count(version, &value) {
                        Ok(byte_count) => byte_count,
                        Err(_) => return resource_internal_failure(request),
                    },
                    values: vec![value],
                }));
            }
            let final_batch_sequence = actions
                .iter()
                .filter_map(|action| match action {
                    ResourceServerFrame::Values(frame) => Some(frame.batch_sequence),
                    _ => None,
                })
                .next_back()
                .unwrap_or(0);
            actions.push_back(ResourceServerFrame::Completed(
                orna_protocol::ResourceCompleted {
                    stream_id,
                    request_id,
                    final_batch_sequence,
                    total_items: actions
                        .iter()
                        .filter(|action| matches!(action, ResourceServerFrame::Values(_)))
                        .count() as u64,
                },
            ));
            actions
        }
        Err(_) => resource_internal_failure(request),
    }
}

fn resource_value_byte_count(
    version: &RawProtocolVersion,
    value: &RuntimeValue,
) -> Result<u32, FrameCodecError> {
    let RawProtocolVersion::Constructed(active, registry) = version else {
        return Err(FrameCodecError::ResourceRequiresConstructed);
    };
    let actual = encode_constructed_value(active, registry, value)
        .map_err(|source| FrameCodecError::Value { source })?
        .len();
    u32::try_from(actual).map_err(|_| FrameCodecError::PayloadTooLarge {
        actual,
        maximum: MAX_FRAME_PAYLOAD_LENGTH,
    })
}

fn resource_action_request_id(action: &ResourceServerFrame) -> orna_core::InvocationId {
    match action {
        ResourceServerFrame::Accepted(frame) => frame.request_id,
        ResourceServerFrame::Values(frame) => frame.request_id,
        ResourceServerFrame::Completed(frame) => frame.request_id,
        ResourceServerFrame::Failed(frame) => frame.request_id,
        ResourceServerFrame::Cancelled(frame) => frame.request_id,
    }
}

fn resource_internal_failure(request: &ResourceRequest) -> VecDeque<ResourceServerFrame> {
    VecDeque::from([ResourceServerFrame::Failed(orna_protocol::ResourceFailed {
        stream_id: request.stream_id,
        request_id: request.request_id,
        failure: CallFailure::InternalFailure,
    })])
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
    let (resource_completion_sender, mut resource_completion_receiver) =
        mpsc::unbounded_channel::<(u64, ResourceDispatchCompletion)>();
    let mut connection = ProtocolConnection::new();
    let mut resource_connection = ResourceProtocolConnection::new();
    let mut retained_payload = BTreeMap::<u64, Vec<PayloadReservation>>::new();
    let mut cancelled = BTreeSet::<u64>::new();
    let mut pending = BTreeMap::<u64, DispatchCompletion>::new();
    let mut resource_cancelled = BTreeMap::<u64, orna_protocol::ResourceCancel>::new();
    let mut resource_pending = BTreeMap::<u64, ResourceDispatchCompletion>::new();
    let mut resource_tasks = BTreeMap::<u64, ResourceTask>::new();
    let mut resource_requests = BTreeMap::<u64, ResourceRequest>::new();
    let mut tasks = JoinSet::<(u64, DispatchCompletion)>::new();
    let mut unstarted = VecDeque::<UnstartedDispatch>::new();
    let result = loop {
        match flush_resource_pending(
            &version,
            &mut resource_connection,
            &mut resource_pending,
            &mut resource_requests,
            &mut writer,
            &mut shutdown,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => break Ok(()),
            Err(error) => break Err(error),
        }
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
            ResourceCompletion(Option<(u64, ResourceDispatchCompletion)>),
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
                    resource = resource_completion_receiver.recv() => Next::ResourceCompletion(resource),
                    () = tokio::task::yield_now() => Next::Start,
                }
            } else {
                Next::Start
            }
        } else if tasks.is_empty() {
            tokio::select! {
                _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                frame = frame_receiver.recv() => Next::Frame(frame.unwrap_or(Ok(None))),
                resource = resource_completion_receiver.recv() => Next::ResourceCompletion(resource),
            }
        } else {
            tokio::select! {
                _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                frame = frame_receiver.recv() => {
                    Next::Frame(frame.unwrap_or(Ok(None)))
                }
                resource = resource_completion_receiver.recv() => Next::ResourceCompletion(resource),
                completion = tasks.join_next(), if !tasks.is_empty() => {
                    Next::Completion(completion)
                }
            }
        };

        match next {
            Next::Frame(Ok(Some(incoming))) => {
                let result = match incoming {
                    IncomingFrame::Raw(incoming) => {
                        handle_client_frame(
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
                    }
                    IncomingFrame::Resource { frame, reservation } => {
                        handle_resource_frame(
                            frame,
                            reservation,
                            &dispatcher,
                            &session,
                            &version,
                            &resources,
                            &mut resource_connection,
                            &mut resource_pending,
                            &mut resource_cancelled,
                            &mut resource_tasks,
                            &mut resource_requests,
                            &resource_completion_sender,
                            &mut resource_completion_receiver,
                            &mut writer,
                            &mut shutdown,
                        )
                        .await
                    }
                };
                match result {
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
            Next::ResourceCompletion(Some((stream_id, completion))) => {
                resource_tasks.remove(&stream_id);
                store_resource_completion(
                    stream_id,
                    completion,
                    &mut resource_pending,
                    &mut resource_cancelled,
                );
            }
            Next::ResourceCompletion(None) => {}
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
    for task in resource_tasks.values() {
        task.cancellation.request_cancel();
    }
    for (_, task) in std::mem::take(&mut resource_tasks) {
        let _ = task.handle.await;
    }
    resource_connection.shutdown();
    drain_resource_completions(
        &mut resource_completion_receiver,
        &mut resource_pending,
        &mut resource_cancelled,
        &mut resource_tasks,
    );
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
fn drain_resource_completions(
    completion_receiver: &mut mpsc::UnboundedReceiver<(u64, ResourceDispatchCompletion)>,
    pending: &mut BTreeMap<u64, ResourceDispatchCompletion>,
    cancelled: &mut BTreeMap<u64, orna_protocol::ResourceCancel>,
    tasks: &mut BTreeMap<u64, ResourceTask>,
) -> BTreeSet<u64> {
    let mut completed = BTreeSet::new();
    while let Ok((stream_id, completion)) = completion_receiver.try_recv() {
        tasks.remove(&stream_id);
        if store_resource_completion(stream_id, completion, pending, cancelled) {
            completed.insert(stream_id);
        }
    }
    completed
}

fn store_resource_completion(
    stream_id: u64,
    completion: ResourceDispatchCompletion,
    pending: &mut BTreeMap<u64, ResourceDispatchCompletion>,
    cancelled: &mut BTreeMap<u64, orna_protocol::ResourceCancel>,
) -> bool {
    if let Some(cancel) = cancelled.remove(&stream_id) {
        let pending_is_terminal = pending
            .get(&stream_id)
            .is_some_and(|completion| {
                completion
                    .actions
                    .iter()
                    .any(resource_action_is_terminal)
            });
        if !pending_is_terminal {
            pending.insert(stream_id, cancelled_resource_completion(cancel));
            return true;
        }
    }
    if pending.contains_key(&stream_id) {
        return false;
    }
    let is_terminal = completion
        .actions
        .iter()
        .any(resource_action_is_terminal);
    pending.insert(stream_id, completion);
    is_terminal
}

fn cancelled_resource_completion(
    cancel: orna_protocol::ResourceCancel,
) -> ResourceDispatchCompletion {
    ResourceDispatchCompletion {
        actions: VecDeque::from([ResourceServerFrame::Cancelled(
            orna_protocol::ResourceCancelled {
                stream_id: cancel.stream_id,
                request_id: cancel.request_id,
                reason: cancel.reason,
            },
        )]),
    }
}
fn resource_action_is_terminal(action: &ResourceServerFrame) -> bool {
    matches!(
        action,
        ResourceServerFrame::Completed(_)
            | ResourceServerFrame::Failed(_)
            | ResourceServerFrame::Cancelled(_)
    )
}
fn resource_completion_is_terminal_for(
    completion: &ResourceDispatchCompletion,
    request_id: orna_core::InvocationId,
) -> bool {
    completion.actions.iter().any(|action| {
        resource_action_is_terminal(action) && resource_action_request_id(action) == request_id
    })
}


#[allow(clippy::too_many_arguments)]
async fn handle_resource_frame<D: DispatchService>(
    frame: ResourceClientFrame,
    _reservation: PayloadReservation,
    dispatcher: &D,
    session: &AuthenticatedSession,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    connection: &mut ResourceProtocolConnection,
    pending: &mut BTreeMap<u64, ResourceDispatchCompletion>,
    cancelled: &mut BTreeMap<u64, orna_protocol::ResourceCancel>,
    tasks: &mut BTreeMap<u64, ResourceTask>,
    requests: &mut BTreeMap<u64, ResourceRequest>,
    completion_sender: &mpsc::UnboundedSender<(u64, ResourceDispatchCompletion)>,
    completion_receiver: &mut mpsc::UnboundedReceiver<(u64, ResourceDispatchCompletion)>,
    _socket: &mut OwnedWriteHalf,
    _shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let request = match &frame {
        ResourceClientFrame::Request(request) => Some(request.clone()),
        _ => None,
    };
    let cancellation = match &frame {
        ResourceClientFrame::Cancel(cancel) => Some(*cancel),
        _ => None,
    };
    let mut committed_completion = cancellation.is_some_and(|cancel| {
        pending
            .get(&cancel.stream_id)
            .is_some_and(|completion| resource_completion_is_terminal_for(completion, cancel.request_id))
    });
    let mut cancellation_won = false;
    if let Some(cancel) = cancellation.filter(|_| !committed_completion) {
        let completed = drain_resource_completions(
            completion_receiver,
            pending,
            cancelled,
            tasks,
        );
        committed_completion = completed.contains(&cancel.stream_id)
            || pending
                .get(&cancel.stream_id)
                .is_some_and(|completion| {
                    resource_completion_is_terminal_for(completion, cancel.request_id)
                });
        if !committed_completion && !cancelled.contains_key(&cancel.stream_id) {
            let Some(request) = requests.get(&cancel.stream_id) else {
                return Err(LocalRawSocketError::ResourceConnection {
                    source: ResourceConnectionError::UnknownStream {
                        stream_id: cancel.stream_id,
                    },
                });
            };
            if request.request_id != cancel.request_id {
                return Err(LocalRawSocketError::ResourceConnection {
                    source: ResourceConnectionError::MismatchedRequest {
                        stream_id: cancel.stream_id,
                    },
                });
            }
            let Some(task) = tasks.get(&cancel.stream_id) else {
                return Err(LocalRawSocketError::ResourceConnection {
                    source: ResourceConnectionError::UnknownStream {
                        stream_id: cancel.stream_id,
                    },
                });
            };
            if !task.cancellation.request_cancel() {
                return Ok(true);
            }
            cancellation_won = true;
        }
    }
    let invalid_scalar_window_update = match &frame {
        ResourceClientFrame::WindowUpdate(update) => requests
            .get(&update.stream_id)
            .is_some_and(|request| {
                request.request_id == update.request_id
                    && request.resource_kind == ResourceKind::Single
            }),
        _ => false,
    };
    let disposition = if committed_completion {
        ResourceFrameDisposition::Applied
    } else {
        match connection.receive(frame) {
            Ok(disposition) => disposition,
            Err(ResourceConnectionError::WrongState { stream_id })
                if invalid_scalar_window_update => {
                    if let Some(task) = tasks.get(&stream_id) {
                        task.cancellation.request_cancel();
                    }
                    let request = requests
                        .get(&stream_id)
                        .expect("scalar resource request exists")
                        .clone();
                    pending.insert(
                        stream_id,
                        ResourceDispatchCompletion {
                            actions: resource_internal_failure(&request),
                        },
                    );
                    ResourceFrameDisposition::Applied
                }
            Err(source) => return Err(LocalRawSocketError::ResourceConnection { source }),
        }
    };
    if let Some(cancel) = cancellation.filter(|_| cancellation_won)
        && matches!(disposition, ResourceFrameDisposition::Applied)
    {
        cancelled.insert(cancel.stream_id, cancel);
    }
    if matches!(disposition, ResourceFrameDisposition::DroppedLate) {
        return Ok(true);
    }
    if let Some(request) = request {
        if let Some(StartedResourceDispatch {
            future,
            cancellation,
        }) = dispatcher.start_resource(
            session.clone(),
            request.clone(),
            resources.clone(),
            version.clone(),
        ) {
            let stream_id = request.stream_id;
            let sender = completion_sender.clone();
            let task_cancellation = cancellation.clone();
            let handle = tokio::spawn(async move {
                let completion = future.await;
                let completion = if task_cancellation.is_requested() {
                    ResourceDispatchCompletion {
                        actions: VecDeque::new(),
                    }
                } else {
                    completion
                };
                let _ = sender.send((stream_id, completion));
            });
            requests.insert(stream_id, request.clone());
            tasks.insert(
                stream_id,
                ResourceTask {
                    handle,
                    cancellation,
                },
            );
        } else {
            pending.insert(
                request.stream_id,
                ResourceDispatchCompletion {
                    actions: resource_internal_failure(&request),
                },
            );
        }
    }
    Ok(true)
}
async fn flush_resource_pending(
    version: &RawProtocolVersion,
    connection: &mut ResourceProtocolConnection,
    pending: &mut BTreeMap<u64, ResourceDispatchCompletion>,
    requests: &mut BTreeMap<u64, ResourceRequest>,
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
            let disposition = match &action {
                ResourceServerFrame::Cancelled(frame) => {
                    match connection.apply_cancelled_after_client_cancel(*frame) {
                        Ok(disposition) => disposition,
                        Err(ResourceConnectionError::InsufficientCredit { .. }) => break,
                        Err(source) => {
                            return Err(LocalRawSocketError::ResourceConnection { source });
                        }
                    }
                }
                _ => match connection.apply(action.clone()) {
                    Ok(disposition) => disposition,
                    Err(ResourceConnectionError::InsufficientCredit { .. }) => break,
                    Err(source) => {
                        return Err(LocalRawSocketError::ResourceConnection { source });
                    }
                },
            };
            match disposition {
                ResourceFrameDisposition::DroppedLate => {
                    let terminal = resource_action_is_terminal(&action);
                    pending
                        .get_mut(&stream_id)
                        .expect("pending resource completion exists")
                        .actions
                        .pop_front();
                    if terminal {
                        requests.remove(&stream_id);
                    }
                    continue;
                }
                ResourceFrameDisposition::Applied => {}
            }
            let encoded = version
                .encode_resource_server_frame(&action)
                .map_err(|source| LocalRawSocketError::Frame { source })?;
            if !write_encoded_frame(stream, &encoded, shutdown).await? {
                return Ok(false);
            }
            let terminal = resource_action_is_terminal(&action);
            pending
                .get_mut(&stream_id)
                .expect("pending resource completion exists")
                .actions
                .pop_front();
            if terminal {
                requests.remove(&stream_id);
            }
        }
    }
    Ok(true)
}

async fn write_encoded_frame(
    stream: &mut OwnedWriteHalf,
    encoded: &[u8],
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    tokio::select! {
        biased;
        _ = wait_for_shutdown(shutdown) => Ok(false),
        result = stream.write_all(encoded) => {
            result
                .map(|()| true)
                .map_err(|source| LocalRawSocketError::Io { source })
        }
    }
}
#[allow(clippy::too_many_arguments)]
async fn handle_client_frame<D: DispatchService>(
    incoming: RawIncomingFrame,
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
    let mut header = [0_u8; RESOURCE_HEADER_LENGTH];
    let Some(()) =
        read_header_before(stream, &mut header[..RESOURCE_MARKER.len()], deadline).await?
    else {
        return Ok(None);
    };
    let resource = &header[..RESOURCE_MARKER.len()] == RESOURCE_MARKER;
    let header_length = if resource {
        RESOURCE_HEADER_LENGTH
    } else {
        FRAME_HEADER_LENGTH
    };
    read_exact_before(
        stream,
        &mut header[RESOURCE_MARKER.len()..header_length],
        deadline,
        LocalRawSocketError::FrameTimeout,
    )
    .await?;
    let declared_offset = if resource { 17..21 } else { 14..18 };
    let declared = u32::from_be_bytes(
        header[declared_offset]
            .try_into()
            .expect("fixed frame header"),
    ) as usize;
    if declared > MAX_FRAME_PAYLOAD_LENGTH {
        return Err(LocalRawSocketError::Frame {
            source: FrameCodecError::PayloadTooLarge {
                actual: declared,
                maximum: MAX_FRAME_PAYLOAD_LENGTH,
            },
        });
    }
    let reservation = resources.reserve_payload(declared)?;
    let mut encoded = Vec::with_capacity(header_length + declared);
    encoded.extend_from_slice(&header[..header_length]);
    encoded.resize(header_length + declared, 0);
    read_exact_before(
        stream,
        &mut encoded[header_length..],
        deadline,
        LocalRawSocketError::FrameTimeout,
    )
    .await?;
    if resource {
        let frame = version
            .decode_resource_client_frame(&encoded)
            .map_err(|source| LocalRawSocketError::Frame { source })?;
        Ok(Some(IncomingFrame::Resource { frame, reservation }))
    } else {
        let frame = version
            .decode_client_frame(&encoded)
            .map_err(|source| LocalRawSocketError::Frame { source })?;
        Ok(Some(IncomingFrame::Raw(RawIncomingFrame {
            frame,
            reservation,
        })))
    }
}

async fn read_header_before<R: AsyncRead + Unpin>(
    stream: &mut R,
    header: &mut [u8],
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

fn report_private_dispatch_source(source: &orna_postgres::PostgresKernelError) {
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
        CatalogueRevisionId, FunctionId, InvocationId, PrincipalId, SchemaId, SourceBundleId,
        SourceRevisionId, TypeId,
        canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, SchemaDefinition,
        },
        revision::{ActiveDatabaseRevision, RevisionPair, StoredSourceRevision},
        security::{
            AuthenticatedSession, Principal, PrincipalKind, PrincipalStatus, SecuritySnapshot,
        },
        value::{EnumValue, RuntimeValue},
    };
    use orna_protocol::{
        Channel, ClientFrame, Event, MAX_RESOURCE_WINDOW, ResourceCancel, ResourceCancellationCode,
        ResourceClientFrame, ResourceKind, ResourceRequest, ResourceServerFrame,
        ResourceWindowUpdate, ServerFrame, decode_catalogue_server_frame,
        decode_resource_server_frame, decode_server_frame, encode_catalogue_client_frame,
        encode_client_frame, encode_resource_client_frame,
    };
    use orna_standard::{
        registered_opaque_codecs, retained_standard_library_snapshot,
        verify_standard_library_snapshot,
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

    fn constructed_test_version() -> (RawProtocolVersion, RevisionPair) {
        let source_bundle = SourceBundleId::from_bytes([0x81; 16]);
        let source_revision = SourceRevisionId::from_bytes([0x82; 16]);
        let bundle_hash = source_bundle_digest(&[]).unwrap();
        let source = StoredSourceRevision::new(
            source_bundle,
            source_revision,
            None,
            Vec::new(),
            bundle_hash,
            source_revision_record_digest(source_bundle, None, bundle_hash).unwrap(),
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x83; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[]).unwrap();
        let active = ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let revision = active.pair();
        let standard =
            verify_standard_library_snapshot(retained_standard_library_snapshot().unwrap())
                .unwrap();
        let registry = registered_opaque_codecs(&standard).unwrap();
        (
            RawProtocolVersion::Constructed(Arc::new(active), Arc::new(registry)),
            revision,
        )
    }

    fn resource_request(revision: RevisionPair) -> ResourceRequest {
        ResourceRequest {
            stream_id: 1,
            request_id: InvocationId::from_bytes([0x11; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x12; 16]),
            call_site_id: orna_core::CallSiteId::from_bytes([0x13; 16]),
            target_function_id: FUNCTION,
            target_revision: revision,
            generation: 1,
            resource_kind: ResourceKind::Single,
            arguments: Vec::new(),
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        }
    }

    #[test]
    fn resource_completion_values_declare_exact_encoded_byte_count() {
        let (version, revision) = constructed_test_version();
        let request = resource_request(revision);
        let value = RuntimeValue::Integer(7);
        let expected = match &version {
            RawProtocolVersion::Constructed(active, registry) => {
                orna_protocol::encode_constructed_value(active, registry, &value)
                    .expect("resource value encodes")
                    .len() as u32
            }
            _ => unreachable!("constructed test version"),
        };
        let actions = resource_completion_actions(
            &version,
            &request,
            Ok(orna_postgres::AuthenticatedServerResourceResult::Completed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
                target_revision: request.target_revision,
                resource_kind: request.resource_kind,
                values: vec![value],
            }),
        );

        let Some(ResourceServerFrame::Values(frame)) = actions.get(1) else {
            panic!("resource completion contains a values frame");
        };
        assert_eq!(frame.byte_count, expected);
    }

    async fn read_resource_server_frame(
        stream: &mut UnixStream,
        active: &orna_core::revision::ActiveDatabaseRevision,
        registry: &orna_core::value::OpaqueCodecRegistry,
    ) -> ResourceServerFrame {
        let mut header = [0_u8; RESOURCE_HEADER_LENGTH];
        stream.read_exact(&mut header).await.unwrap();
        let payload_length = u32::from_be_bytes(header[17..21].try_into().unwrap()) as usize;
        let mut encoded = header.to_vec();
        encoded.resize(RESOURCE_HEADER_LENGTH + payload_length, 0);
        stream
            .read_exact(&mut encoded[RESOURCE_HEADER_LENGTH..])
            .await
            .unwrap();
        decode_resource_server_frame(active, registry, &encoded).unwrap()
    }

    fn resource_actions(
        version: &RawProtocolVersion,
        request: &ResourceRequest,
        values: Vec<RuntimeValue>,
    ) -> VecDeque<ResourceServerFrame> {
        let total_items = values.len() as u64;
        let final_batch_sequence = total_items.saturating_sub(1);
        let mut actions = VecDeque::with_capacity(values.len() + 2);
        actions.push_back(ResourceServerFrame::Accepted(
            orna_protocol::ResourceAccepted {
                stream_id: request.stream_id,
                request_id: request.request_id,
                nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
                target_revision: request.target_revision,
                resource_kind: request.resource_kind,
            },
        ));
        for (batch_sequence, value) in values.into_iter().enumerate() {
            actions.push_back(ResourceServerFrame::Values(orna_protocol::ResourceValues {
                stream_id: request.stream_id,
                request_id: request.request_id,
                batch_sequence: batch_sequence as u64,
                item_count: 1,
                byte_count: resource_value_byte_count(version, &value)
                    .expect("resource value encodes"),
                values: vec![value],
            }));
        }
        actions.push_back(ResourceServerFrame::Completed(
            orna_protocol::ResourceCompleted {
                stream_id: request.stream_id,
                request_id: request.request_id,
                final_batch_sequence,
                total_items,
            },
        ));
        actions
    }

    #[derive(Clone)]
    struct ResourceDispatch;

    impl DispatchService for ResourceDispatch {
        fn start(
            &self,
            _session: AuthenticatedSession,
            _stream: u64,
            _call: RawCall,
        ) -> StartedDispatch {
            panic!("resource transport test does not issue a raw call")
        }

        fn start_resource(
            &self,
            _session: AuthenticatedSession,
            request: ResourceRequest,
            _resources: LocalRawSocketResources,
            version: RawProtocolVersion,
        ) -> Option<StartedResourceDispatch> {
            let actions = resource_actions(&version, &request, vec![RuntimeValue::Integer(7)]);
            Some(StartedResourceDispatch {
                future: Box::pin(async move { ResourceDispatchCompletion { actions } }),
                cancellation: ResourceCancellation::new(),
            })
        }
    }

    #[derive(Clone)]
    struct MultiValueResourceDispatch;

    impl DispatchService for MultiValueResourceDispatch {
        fn start(
            &self,
            _session: AuthenticatedSession,
            _stream: u64,
            _call: RawCall,
        ) -> StartedDispatch {
            panic!("resource transport test does not issue a raw call")
        }

        fn start_resource(
            &self,
            _session: AuthenticatedSession,
            request: ResourceRequest,
            _resources: LocalRawSocketResources,
            version: RawProtocolVersion,
        ) -> Option<StartedResourceDispatch> {
            let actions = resource_actions(
                &version,
                &request,
                vec![RuntimeValue::Integer(7), RuntimeValue::Integer(8)],
            );
            Some(StartedResourceDispatch {
                future: Box::pin(async move { ResourceDispatchCompletion { actions } }),
                cancellation: ResourceCancellation::new(),
            })
        }
    }

    #[derive(Clone)]
    struct BlockingResourceDispatch {
        started: Arc<Notify>,
        cancelled: Arc<AtomicBool>,
    }

    impl DispatchService for BlockingResourceDispatch {
        fn start(
            &self,
            _session: AuthenticatedSession,
            _stream: u64,
            _call: RawCall,
        ) -> StartedDispatch {
            panic!("resource transport test does not issue a raw call")
        }

        fn start_resource(
            &self,
            _session: AuthenticatedSession,
            _request: ResourceRequest,
            _resources: LocalRawSocketResources,
            _version: RawProtocolVersion,
        ) -> Option<StartedResourceDispatch> {
            let started = Arc::clone(&self.started);
            let cancelled = Arc::clone(&self.cancelled);
            let cancellation = ResourceCancellation::new();
            let operation_cancellation = cancellation.clone();
            Some(StartedResourceDispatch {
                future: Box::pin(async move {
                    started.notify_one();
                    tokio::select! {
                        _ = operation_cancellation.cancelled() => {
                            cancelled.store(true, Ordering::SeqCst);
                            ResourceDispatchCompletion {
                                actions: VecDeque::new(),
                            }
                        }
                        completion = std::future::pending::<ResourceDispatchCompletion>() => completion,
                    }
                }),
                cancellation,
            })
        }
    }


    #[derive(Clone)]
    struct MixedResourceDispatch {
        started: Arc<Notify>,
    }

    impl DispatchService for MixedResourceDispatch {
        fn start(
            &self,
            _session: AuthenticatedSession,
            _stream: u64,
            _call: RawCall,
        ) -> StartedDispatch {
            panic!("resource transport test does not issue a raw call")
        }

        fn start_resource(
            &self,
            _session: AuthenticatedSession,
            request: ResourceRequest,
            _resources: LocalRawSocketResources,
            version: RawProtocolVersion,
        ) -> Option<StartedResourceDispatch> {
            if request.stream_id == 1 {
                let started = Arc::clone(&self.started);
                let cancellation = ResourceCancellation::new();
                let operation_cancellation = cancellation.clone();
                return Some(StartedResourceDispatch {
                    future: Box::pin(async move {
                        started.notify_one();
                        tokio::select! {
                            _ = operation_cancellation.cancelled() => {
                                ResourceDispatchCompletion {
                                    actions: VecDeque::new(),
                                }
                            }
                            completion = std::future::pending::<ResourceDispatchCompletion>() => completion,
                        }
                    }),
                    cancellation,
                });
            }
            let actions = resource_actions(
                &version,
                &request,
                vec![RuntimeValue::Integer(7), RuntimeValue::Integer(8)],
            );
            Some(StartedResourceDispatch {
                future: Box::pin(async move { ResourceDispatchCompletion { actions } }),
                cancellation: ResourceCancellation::new(),
            })
        }
    }

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[derive(Clone)]
    struct ShutdownResourceDispatch {
        started: Arc<Notify>,
        dropped: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
    }

    impl DispatchService for ShutdownResourceDispatch {
        fn start(
            &self,
            _session: AuthenticatedSession,
            _stream: u64,
            _call: RawCall,
        ) -> StartedDispatch {
            panic!("resource transport test does not issue a raw call")
        }

        fn start_resource(
            &self,
            _session: AuthenticatedSession,
            _request: ResourceRequest,
            _resources: LocalRawSocketResources,
            _version: RawProtocolVersion,
        ) -> Option<StartedResourceDispatch> {
            let started = Arc::clone(&self.started);
            let dropped = Arc::clone(&self.dropped);
            let cancelled = Arc::clone(&self.cancelled);
            let cancellation = ResourceCancellation::new();
            let operation_cancellation = cancellation.clone();
            Some(StartedResourceDispatch {
                future: Box::pin(async move {
                    let _drop_signal = DropSignal(dropped);
                    started.notify_one();
                    tokio::select! {
                        _ = operation_cancellation.cancelled() => {
                            cancelled.store(true, Ordering::SeqCst);
                            ResourceDispatchCompletion {
                                actions: VecDeque::new(),
                            }
                        }
                        completion = std::future::pending::<ResourceDispatchCompletion>() => completion,
                    }
                }),
                cancellation,
            })
        }
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
    async fn queued_resource_permit_waiter_completes_when_cancelled() {
        let resources = LocalRawSocketResources::new();
        let held: Vec<_> = (0..KERNEL_OPERATION_LIMIT)
            .map(|_| resources.reserve_kernel_operation().expect("operation permit"))
            .collect();
        let cancellation = ResourceCancellation::new();
        let waiter = tokio::spawn({
            let resources = resources.clone();
            let cancellation = cancellation.clone();
            async move { resources.acquire_kernel_operation(&cancellation).await }
        });

        tokio::task::yield_now().await;
        assert_eq!(resources.kernel_operations.available_permits(), 0);
        assert!(cancellation.request_cancel());
        let permit = timeout(Duration::from_millis(50), waiter)
            .await
            .expect("queued resource waiter joins after cancellation")
            .expect("queued resource waiter task");
        assert!(permit.is_none());
        assert_eq!(resources.kernel_operations.available_permits(), 0);
        drop(held);
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT
        );
    }

    #[tokio::test]
    async fn constructed_resource_request_delivers_a_scalar_result() {
        let (version, revision) = constructed_test_version();
        let (active, registry) = match &version {
            RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
            _ => unreachable!("constructed test version"),
        };
        let request = resource_request(revision);
        let request_id = request.request_id;
        let encoded = encode_resource_client_frame(
            &active,
            &registry,
            &ResourceClientFrame::Request(request),
        )
        .unwrap();
        let resources = LocalRawSocketResources::new();
        let (server, mut client) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
            ResourceDispatch,
            test_session(),
            version,
            server,
            resources,
            watch::channel(false).1,
        ));

        client.write_all(&encoded).await.unwrap();
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Accepted(_)
        ));
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Values(frame)
                if frame.values == vec![RuntimeValue::Integer(7)]
                    && frame.item_count == 1
                    && frame.batch_sequence == 0
        ));
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Completed(frame)
                if frame.final_batch_sequence == 0 && frame.total_items == 1
        ));
        let cancel = encode_resource_client_frame(
            &active,
            &registry,
            &ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: 1,
                request_id,
                reason: ResourceCancellationCode::ClientRequested,
            }),
        )
        .unwrap();
        client.write_all(&cancel).await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(50),
                read_resource_server_frame(&mut client, &active, &registry),
            )
            .await
            .is_err()
        );

        client.shutdown().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn invalid_scalar_window_update_only_terminates_its_resource_stream() {
        let (version, revision) = constructed_test_version();
        let (active, registry) = match &version {
            RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
            _ => unreachable!("constructed test version"),
        };
        let started = Arc::new(Notify::new());
        let dispatcher = MixedResourceDispatch {
            started: Arc::clone(&started),
        };
        let (server, mut client) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
            dispatcher,
            test_session(),
            version,
            server,
            LocalRawSocketResources::new(),
            watch::channel(false).1,
        ));

        let scalar = resource_request(revision);
        let scalar_request_id = scalar.request_id;
        client
            .write_all(
                &encode_resource_client_frame(
                    &active,
                    &registry,
                    &ResourceClientFrame::Request(scalar),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        started.notified().await;

        client
            .write_all(
                &encode_resource_client_frame(
                    &active,
                    &registry,
                    &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                        stream_id: 1,
                        request_id: scalar_request_id,
                        add_items: 1,
                        add_bytes: 1,
                    }),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let scalar_failure = read_resource_server_frame(&mut client, &active, &registry).await;
        assert!(
            matches!(
                scalar_failure,
                ResourceServerFrame::Failed(frame)
                    if frame.stream_id == 1
                        && frame.request_id == scalar_request_id
                        && frame.failure == orna_protocol::CallFailure::InternalFailure
            ),
            "unexpected scalar failure frame: {scalar_failure:?}"
        );

        let mut unrelated = resource_request(revision);
        unrelated.stream_id = 2;
        unrelated.request_id = InvocationId::from_bytes([0x22; 16]);
        unrelated.resource_kind = ResourceKind::Stream;
        unrelated.item_window = 1;
        let value_bytes =
            orna_protocol::encode_constructed_value(&active, &registry, &RuntimeValue::Integer(7))
                .unwrap();
        unrelated.byte_window = value_bytes.len() as u64;
        client
            .write_all(
                &encode_resource_client_frame(
                    &active,
                    &registry,
                    &ResourceClientFrame::Request(unrelated.clone()),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Accepted(frame)
                if frame.stream_id == unrelated.stream_id
                    && frame.request_id == unrelated.request_id
        ));
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Values(frame)
                if frame.stream_id == unrelated.stream_id
                    && frame.values == vec![RuntimeValue::Integer(7)]
        ));

        client
            .write_all(
                &encode_resource_client_frame(
                    &active,
                    &registry,
                    &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                        stream_id: unrelated.stream_id,
                        request_id: unrelated.request_id,
                        add_items: 1,
                        add_bytes: value_bytes.len() as u64,
                    }),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Values(frame)
                if frame.stream_id == unrelated.stream_id
                    && frame.values == vec![RuntimeValue::Integer(8)]
        ));
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Completed(frame)
                if frame.stream_id == unrelated.stream_id
                    && frame.total_items == 2
        ));

        client.shutdown().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn constructed_resource_stream_resumes_after_window_update() {
        let (version, revision) = constructed_test_version();
        let (active, registry) = match &version {
            RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
            _ => unreachable!("constructed test version"),
        };
        let value_bytes =
            orna_protocol::encode_constructed_value(&active, &registry, &RuntimeValue::Integer(7))
                .unwrap();
        let mut request = resource_request(revision);
        request.resource_kind = ResourceKind::Stream;
        request.item_window = 1;
        request.byte_window = value_bytes.len() as u64;
        let request_id = request.request_id;
        let encoded = encode_resource_client_frame(
            &active,
            &registry,
            &ResourceClientFrame::Request(request),
        )
        .unwrap();
        let (server, mut client) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
            MultiValueResourceDispatch,
            test_session(),
            version,
            server,
            LocalRawSocketResources::new(),
            watch::channel(false).1,
        ));

        client.write_all(&encoded).await.unwrap();
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Accepted(_)
        ));
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Values(frame)
                if frame.values == vec![RuntimeValue::Integer(7)]
                    && frame.item_count == 1
                    && frame.byte_count == value_bytes.len() as u32
        ));
        assert!(
            timeout(
                Duration::from_millis(50),
                read_resource_server_frame(&mut client, &active, &registry),
            )
            .await
            .is_err()
        );

        let cancel = encode_resource_client_frame(
            &active,
            &registry,
            &ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: 1,
                request_id,
                reason: ResourceCancellationCode::ClientRequested,
            }),
        )
        .unwrap();
        client.write_all(&cancel).await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(50),
                read_resource_server_frame(&mut client, &active, &registry),
            )
            .await
            .is_err(),
            "a committed completion must win over a later cancellation"
        );

        let update = encode_resource_client_frame(
            &active,
            &registry,
            &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                stream_id: 1,
                request_id,
                add_items: 1,
                add_bytes: value_bytes.len() as u64,
            }),
        )
        .unwrap();
        client.write_all(&update).await.unwrap();
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Values(frame)
                if frame.values == vec![RuntimeValue::Integer(8)]
                    && frame.batch_sequence == 1
                    && frame.item_count == 1
                    && frame.byte_count == value_bytes.len() as u32
        ));
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Completed(frame)
                if frame.final_batch_sequence == 1 && frame.total_items == 2
        ));

        client.shutdown().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn constructed_resource_cancellation_wins_before_dispatch_completion() {
        let (version, revision) = constructed_test_version();
        let (active, registry) = match &version {
            RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
            _ => unreachable!("constructed test version"),
        };
        let request = resource_request(revision);
        let request_id = request.request_id;
        let encoded = encode_resource_client_frame(
            &active,
            &registry,
            &ResourceClientFrame::Request(request),
        )
        .unwrap();
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (server, mut client) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
            BlockingResourceDispatch {
                started: Arc::clone(&started),
                cancelled: Arc::clone(&cancelled),
            },
            test_session(),
            version,
            server,
            LocalRawSocketResources::new(),
            watch::channel(false).1,
        ));

        client.write_all(&encoded).await.unwrap();
        started.notified().await;
        let cancel = encode_resource_client_frame(
            &active,
            &registry,
            &ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: 1,
                request_id,
                reason: ResourceCancellationCode::ClientRequested,
            }),
        )
        .unwrap();
        client.write_all(&cancel).await.unwrap();
        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Cancelled(frame)
                if frame.stream_id == 1
                    && frame.request_id == request_id
                    && frame.reason == ResourceCancellationCode::ClientRequested
        ));
        assert!(cancelled.load(Ordering::SeqCst));

        client.shutdown().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn queued_resource_completion_wins_over_cancellation() {
        let (version, revision) = constructed_test_version();
        let request = resource_request(revision);
        let (server, _client) = UnixStream::pair().unwrap();
        let (_reader, mut writer) = server.into_split();
        let (completion_sender, mut completion_receiver) =
            mpsc::unbounded_channel::<(u64, ResourceDispatchCompletion)>();
        let mut connection = ResourceProtocolConnection::new();
        let mut pending = BTreeMap::new();
        let mut cancelled = BTreeMap::new();
        let mut tasks: BTreeMap<u64, ResourceTask> = BTreeMap::new();
        let mut requests = BTreeMap::new();
        let resources = LocalRawSocketResources::new();
        let (_shutdown_sender, mut shutdown) = watch::channel(false);
        let hook_called = Arc::new(AtomicBool::new(false));
        let dispatcher = BlockingResourceDispatch {
            started: Arc::new(Notify::new()),
            cancelled: Arc::clone(&hook_called),
        };

        handle_resource_frame(
            ResourceClientFrame::Request(request.clone()),
            PayloadReservation { _permit: None },
            &dispatcher,
            &test_session(),
            &version,
            &resources,
            &mut connection,
            &mut pending,
            &mut cancelled,
            &mut tasks,
            &mut requests,
            &completion_sender,
            &mut completion_receiver,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap();
        completion_sender
            .send((
                request.stream_id,
                ResourceDispatchCompletion {
                    actions: resource_actions(&version, &request, vec![RuntimeValue::Integer(7)]),
                },
            ))
            .unwrap();

        handle_resource_frame(
            ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: request.stream_id,
                request_id: request.request_id,
                reason: ResourceCancellationCode::ClientRequested,
            }),
            PayloadReservation { _permit: None },
            &dispatcher,
            &test_session(),
            &version,
            &resources,
            &mut connection,
            &mut pending,
            &mut cancelled,
            &mut tasks,
            &mut requests,
            &completion_sender,
            &mut completion_receiver,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap();

        assert!(matches!(
            pending
                .get(&request.stream_id)
                .and_then(|completion| completion.actions.front()),
            Some(ResourceServerFrame::Accepted(_))
        ));
        assert!(requests.contains_key(&request.stream_id));
        assert!(!hook_called.load(Ordering::SeqCst));
        assert!(flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap());
        assert!(pending.is_empty());
        assert!(!requests.contains_key(&request.stream_id));
    }
    #[tokio::test]
    async fn committing_resource_cancellation_does_not_terminalise_stream() {
        let (version, revision) = constructed_test_version();
        let request = resource_request(revision);
        let (server, _client) = UnixStream::pair().unwrap();
        let (_reader, mut writer) = server.into_split();
        let (completion_sender, mut completion_receiver) =
            mpsc::unbounded_channel::<(u64, ResourceDispatchCompletion)>();
        let mut connection = ResourceProtocolConnection::new();
        let mut pending = BTreeMap::new();
        let mut cancelled = BTreeMap::new();
        let mut tasks: BTreeMap<u64, ResourceTask> = BTreeMap::new();
        let mut requests = BTreeMap::new();
        let resources = LocalRawSocketResources::new();
        let (_shutdown_sender, mut shutdown) = watch::channel(false);
        let dispatcher = BlockingResourceDispatch {
            started: Arc::new(Notify::new()),
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        handle_resource_frame(
            ResourceClientFrame::Request(request.clone()),
            PayloadReservation { _permit: None },
            &dispatcher,
            &test_session(),
            &version,
            &resources,
            &mut connection,
            &mut pending,
            &mut cancelled,
            &mut tasks,
            &mut requests,
            &completion_sender,
            &mut completion_receiver,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap();

        let cancellation = tasks
            .get(&request.stream_id)
            .expect("resource task")
            .cancellation
            .clone();
        assert!(cancellation.try_begin_commit());

        handle_resource_frame(
            ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: request.stream_id,
                request_id: request.request_id,
                reason: ResourceCancellationCode::ClientRequested,
            }),
            PayloadReservation { _permit: None },
            &dispatcher,
            &test_session(),
            &version,
            &resources,
            &mut connection,
            &mut pending,
            &mut cancelled,
            &mut tasks,
            &mut requests,
            &completion_sender,
            &mut completion_receiver,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap();

        assert!(cancelled.is_empty());
        tasks
            .remove(&request.stream_id)
            .expect("resource task")
            .handle
            .abort();
        completion_sender
            .send((
                request.stream_id,
                ResourceDispatchCompletion {
                    actions: resource_actions(
                        &version,
                        &request,
                        vec![RuntimeValue::Integer(7)],
                    ),
                },
            ))
            .unwrap();
        assert!(
            drain_resource_completions(
                &mut completion_receiver,
                &mut pending,
                &mut cancelled,
                &mut tasks,
            )
            .contains(&request.stream_id)
        );
        assert!(flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap());
        assert!(pending.is_empty());
    }


    #[tokio::test]
    async fn server_shutdown_cancels_active_resource_without_emitting_a_terminal_frame() {
        let (version, revision) = constructed_test_version();
        let (active, registry) = match &version {
            RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
            _ => unreachable!("constructed test version"),
        };
        let encoded = encode_resource_client_frame(
            &active,
            &registry,
            &ResourceClientFrame::Request(resource_request(revision)),
        )
        .unwrap();
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let dispatcher = ShutdownResourceDispatch {
            started: Arc::clone(&started),
            dropped: Arc::clone(&dropped),
            cancelled: Arc::clone(&cancelled),
        };
        let (shutdown_sender, shutdown) = watch::channel(false);
        let (server, mut client) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
            dispatcher,
            test_session(),
            version,
            server,
            LocalRawSocketResources::new(),
            shutdown,
        ));

        client.write_all(&encoded).await.unwrap();
        started.notified().await;
        shutdown_sender.send(true).unwrap();
        server_task.await.unwrap().unwrap();

        let mut byte = [0_u8; 1];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(cancelled.load(Ordering::SeqCst));
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
