#![allow(clippy::collapsible_if)]
#![allow(clippy::let_unit_value)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::nonminimal_bool)]
// Raw socket protocol preserves accepted carrier and error layouts.
#![allow(clippy::large_enum_variant)]
// Raw socket operations return the stable embedded-host error boundary.
#![allow(clippy::result_large_err)]
#![allow(clippy::too_many_arguments)]
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
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

use orna_client::ClientStateStore;
#[cfg(test)]
use orna_core::value::RuntimeValue;
use orna_core::{
    InvocationId,
    catalogue::CatalogueSnapshot,
    invocation::{
        InvocationEventBody, InvocationEventKind, InvocationFailure, InvocationFailurePhase,
        InvocationRetryability, InvokeEvent, InvokeValue,
    },
    revision::{ActiveDatabaseRevision, RevisionPair},
    security::AuthenticatedSession,
    value::OpaqueCodecRegistry,
};
use orna_postgres::{
    AuthenticatedServerResourceAccepted, AuthenticatedServerResourceEvent,
    AuthenticatedServerResourceKind, AuthenticatedServerResourceProducer,
    AuthenticatedServerResourceStart, PostgresKernel, PostgresKernelError, ResourceCancellation,
    ResourceCredit, SealedInvocationContinuation, SealedInvocationExecution,
    SealedInvocationPreflight, SealedInvocationResult,
};
#[cfg(test)]
use orna_protocol::encode_constructed_value;
use orna_protocol::{
    CallFailure, ClientAction, ClientFrame, ConnectionError, FrameCodecError, InvocationEventBatch,
    InvocationEventRecord, MAX_FRAME_PAYLOAD_LENGTH, ProtocolConnection, RawCall,
    ResourceClientFrame, ResourceConnectionError, ResourceFrameDisposition, ResourceKind,
    ResourceProtocolConnection, ResourceRequest, ResourceServerFrame, SESSION_HEADER_LENGTH,
    SESSION_MARKER, ServerAction, ServerFrame, SessionClientFrame, SessionCodecError,
    decode_active_client_frame, decode_catalogue_client_frame, decode_client_frame,
    decode_constructed_client_frame, decode_registered_client_frame, decode_resource_client_frame,
    decode_session_client_frame, encode_active_server_frame, encode_catalogue_server_frame,
    encode_constructed_server_frame, encode_registered_server_frame, encode_resource_server_frame,
    encode_server_frame, encode_session_server_frame,
};
use orna_standard::{RegisteredOpaqueCodecsError, registered_opaque_codecs};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::{JoinError, JoinHandle, JoinSet},
    time::{Instant, timeout, timeout_at},
};

#[cfg(feature = "test-hooks")]
use crate::invoke::RawResourceRequestAuthorizer;
use crate::invoke::{ResourceTerminalProvenance, SharedInvokeBroker};
use crate::{InstalledClientResourceExecutor, RawClientDispatch, authenticate_local_stream};

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
const RESOURCE_MARKER: &[u8; 15] = b"ORNA-RESOURCE/1";
const RESOURCE_HEADER_LENGTH: usize = 21;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const SHARED_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;
const KERNEL_OPERATION_LIMIT: usize = 64;
const CONNECTION_LIMIT: usize = 64;
// ResourceProtocolConnection admits at most CONNECTION_LIMIT live resources,
// and the scheduler permits at most one completion in flight per live stream.
const RESOURCE_COMPLETION_CHANNEL_CAPACITY: usize = CONNECTION_LIMIT;
const RESOURCE_PRODUCER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_CHANNEL_CAPACITY: usize = 64;
const PREFLIGHT_FRAME_FAIRNESS_BUDGET: usize = 8;
const PENDING_FLUSH_FAIRNESS_BUDGET: usize = 8;
/// Gives cancelled dispatches a bounded cooperative drain window before
/// started workers are joined to completion during connection shutdown.
const DISPATCH_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const SOCKET_NAME: &str = "orna.sock";
type SealedPullTaskResult = (
    u64,
    AuthenticatedServerResourceProducer,
    Result<AuthenticatedServerResourceEvent, PostgresKernelError>,
);
const SEALED_CONNECTION_PROTOCOL_MAJOR: u16 = 5;

#[derive(Clone, Default)]
struct ResourceReadState {
    active: Arc<AtomicBool>,
}

impl ResourceReadState {
    fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::SeqCst);
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

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
    fn apply_resource(
        &self,
        connection: &mut ResourceProtocolConnection,
        frame: ResourceServerFrame,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        match self {
            Self::Constructed(active, registry) => {
                connection.apply_constructed(active, registry, frame)
            }
            Self::One | Self::Catalogue(_) | Self::Active(_) | Self::Registered(_, _) => {
                Err(ResourceConnectionError::InvalidFrame {
                    source: FrameCodecError::ResourceRequiresConstructed,
                })
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
    #[cfg(test)]
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
    /// Local peer authentication did not finish before its deadline.
    AuthenticationTimeout,
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
    /// A client session-control frame violated the bounded session codec.
    Session {
        /// The session frame codec failure.
        source: SessionCodecError,
    },
    /// A client session-control response did not match the active request.
    SessionState {
        /// The session state-machine failure.
        source: orna_protocol::SessionStateError,
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
            Self::AuthenticationTimeout => "local raw socket authentication timed out",
            Self::InvalidHello => "local raw socket handshake is invalid",
            Self::FrameTimeout => "local raw socket frame timed out",
            Self::PayloadCapacity => "local raw socket payload capacity is exhausted",
            Self::KernelCapacity => "local raw socket kernel capacity is exhausted",
            Self::Authentication { .. } => "local raw socket authentication failed",
            Self::Catalogue { .. } => "local raw socket catalogue recovery failed",
            Self::ActiveRevision { .. } => "local raw socket active revision recovery failed",
            Self::OpaqueRegistry { .. } => "local raw socket opaque registry validation failed",
            Self::Frame { .. } => "local raw socket frame is invalid",
            Self::Session { .. } => "local raw socket session frame is invalid",
            Self::SessionState { .. } => "local raw socket session state is invalid",
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
            Self::Session { source } => Some(source),
            Self::SessionState { source } => Some(source),
            Self::Connection { source } => Some(source),
            Self::ResourceConnection { source } => Some(source),
            Self::ResourceCancellationAudit { source } => Some(source),
            Self::DispatchTask { source } => Some(source),
            Self::ConnectionTask { source } => Some(source),
            Self::HandshakeTimeout
            | Self::AuthenticationTimeout
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
    serve_local_raw_stream_with_broker(kernel, stream, resources, None).await
}

pub(crate) async fn serve_local_raw_stream_with_broker(
    kernel: PostgresKernel,
    stream: StandardUnixStream,
    resources: LocalRawSocketResources,
    broker: Option<SharedInvokeBroker>,
) -> Result<(), LocalRawSocketError> {
    let (shutdown_guard, shutdown) = watch::channel(false);
    run_owned_connection_with_shutdown_guard(shutdown_guard, async move {
        negotiate_and_drive(kernel, stream, resources, shutdown, broker).await
    })
    .await
}

/// Serves an installed resource socket with explicit request provenance.
///
/// The public raw socket entry point fails closed because an unbound socket
/// has no trusted client evaluator. This hidden seam exists for integration
/// tests that register each request before they send protocol frames.
#[doc(hidden)]
#[cfg(feature = "test-hooks")]
pub async fn serve_local_raw_stream_with_resource_authorizer(
    kernel: PostgresKernel,
    stream: StandardUnixStream,
    resources: LocalRawSocketResources,
    authorizer: RawResourceRequestAuthorizer,
) -> Result<(), LocalRawSocketError> {
    serve_local_raw_stream_with_broker(kernel, stream, resources, Some(authorizer.broker())).await
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
        let result = negotiate_and_drive(kernel, stream, resources, shutdown, None).await;
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
            return;
        }
    }
}

async fn negotiate_and_drive(
    kernel: PostgresKernel,
    stream: StandardUnixStream,
    resources: LocalRawSocketResources,
    mut shutdown: watch::Receiver<bool>,
    broker: Option<SharedInvokeBroker>,
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
    let session = tokio::select! {
        biased;
        _ = wait_for_shutdown(&mut shutdown) => return Ok(()),
        result = timeout(HANDSHAKE_TIMEOUT, authenticate_local_stream(&kernel, &peer_stream)) => {
            result
                .map_err(|_| LocalRawSocketError::AuthenticationTimeout)?
                .map_err(|source| LocalRawSocketError::Authentication { source })?
        }
    };
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
        RawDispatchService {
            kernel,
            invoke_cancellations: Arc::new(Mutex::new(BTreeMap::new())),
            resource_broker: broker,
        },
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
    Session {
        frame: SessionClientFrame,
        reservation: PayloadReservation,
    },
}

struct DispatchCompletion {
    actions: VecDeque<ServerAction>,
    cancellation: ServerAction,
    cancellation_token: Option<ResourceCancellation>,
    sealed_producer: Option<AuthenticatedServerResourceProducer>,
    /// Root CALL_ACCEPTED identity shared by every sealed invocation event.
    sealed_invocation: Option<InvocationId>,
    sealed_next_event_sequence: u64,
    sealed_next_outer_sequence: u64,
    start_gate: Option<oneshot::Sender<()>>,
    start_delivered: bool,
    terminal_delivered: bool,
    terminal_claimed: bool,
    worker_completed: bool,
    _guards: Option<DispatchGuards>,
}

impl DispatchCompletion {
    /// The bounded pending action queue owns every produced result once the
    /// protected worker has returned, including any terminal action. Release
    /// the request payload budget at that boundary; a sealed producer still
    /// owns a live transaction, so retain only its operation permit until the
    /// producer reaches a terminal state.
    fn release_guards_after_worker_completion(&mut self) {
        if !self.worker_completed {
            return;
        }
        if let Some(guards) = self._guards.as_mut() {
            guards._payload.clear();
        }
        if self.sealed_producer.is_none() {
            self._guards = None;
        }
    }
}

fn cancellation_actions(stream: u64, cancellation: ServerAction) -> VecDeque<ServerAction> {
    match cancellation {
        ServerAction::InvokeCancelled { .. } => VecDeque::from([
            ServerAction::InvokeCancelled { stream },
            ServerAction::Completed { stream },
        ]),
        cancellation => VecDeque::from([cancellation]),
    }
}

fn should_cancel_on_disconnect(completion: &DispatchCompletion) -> bool {
    !completion.terminal_delivered
        && !completion
            .cancellation_token
            .as_ref()
            .is_some_and(ResourceCancellation::is_requested)
        && (completion.sealed_invocation.is_none() || !completion.start_delivered)
}

fn queue_cancellation_actions(completion: &mut DispatchCompletion, stream: u64) {
    completion.sealed_producer.take();
    completion.release_guards_after_worker_completion();
    let cancellation = cancellation_actions(stream, completion.cancellation.clone());
    if completion.start_gate.is_some() && !completion.start_delivered {
        completion.actions.truncate(1);
        completion.actions.extend(cancellation);
    } else {
        completion.actions = cancellation;
    }
}

fn dispatch_completion_has_claimed_terminal(completion: &DispatchCompletion) -> bool {
    let mut terminal = false;
    for action in &completion.actions {
        match action {
            // A clean completion and expected raw-call failures remain
            // replaceable until their terminal frame is delivered. Only an
            // operational failure is protected by the raw-call boundary.
            ServerAction::Failed {
                failure: CallFailure::InternalFailure,
                ..
            } => terminal = true,
            // A sealed invocation terminal Event is already the result of its
            // protected commit. It must not be replaced by a queued cancel,
            // even though its unwindowed CALL_COMPLETED frame is still queued.
            ServerAction::InvokeEvents { events, .. }
                if events.records().iter().any(|record| {
                    matches!(
                        record.event().kind(),
                        InvocationEventKind::InvocationCompleted
                            | InvocationEventKind::InvocationFailed
                            | InvocationEventKind::InvocationCancelled
                    )
                }) =>
            {
                terminal = true
            }
            ServerAction::Cancelled { .. } | ServerAction::InvokeCancelled { .. } => return false,
            ServerAction::Accepted { .. }
            | ServerAction::Events { .. }
            | ServerAction::InvokeEvents { .. }
            | ServerAction::Completed { .. }
            | ServerAction::Failed { .. } => {}
        }
    }
    terminal
}

fn transfer_sealed_completion_state(
    existing: &mut DispatchCompletion,
    completion: &mut DispatchCompletion,
) {
    if let Some(producer) = completion.sealed_producer.take() {
        existing.sealed_producer = Some(producer);
        if existing._guards.is_none() {
            existing._guards = completion._guards.take();
        }
    }
    if completion.cancellation_token.is_some() {
        existing.cancellation_token = completion.cancellation_token.take();
    }
    if let Some(invocation) = completion.sealed_invocation {
        existing.sealed_invocation = Some(invocation);
        existing.sealed_next_event_sequence = completion.sealed_next_event_sequence;
        existing.sealed_next_outer_sequence = completion.sealed_next_outer_sequence;
    }
}

fn merge_dispatch_completion(
    stream_id: u64,
    mut completion: DispatchCompletion,
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    cancelled: &mut BTreeSet<u64>,
    cancelled_pending: &mut BTreeSet<u64>,
) {
    completion.worker_completed = true;
    completion.release_guards_after_worker_completion();
    completion.terminal_claimed = dispatch_completion_has_claimed_terminal(&completion);
    let cancellation_requested = completion
        .cancellation_token
        .as_ref()
        .is_some_and(ResourceCancellation::is_requested)
        && !completion.terminal_claimed;

    // A cancellation queued while the worker was live owns the pending
    // terminal frames unless the worker has already claimed a terminal result.
    if cancelled_pending.remove(&stream_id) {
        if let Some(existing) = pending.get_mut(&stream_id) {
            transfer_sealed_completion_state(existing, &mut completion);
            existing.worker_completed = true;
            existing.release_guards_after_worker_completion();
            if completion.terminal_claimed && !existing.terminal_delivered {
                existing.terminal_claimed = true;
                let started = existing
                    .start_gate
                    .is_some()
                    .then(|| existing.actions.front().cloned())
                    .flatten();
                existing.actions.clear();
                if let Some(started) = started {
                    existing.actions.push_back(started);
                }
                existing.actions.extend(completion.actions);
                existing.cancellation = completion.cancellation;
            }
        } else if completion.sealed_producer.is_some() {
            // The cancellation terminal may have flushed and removed the
            // pending entry before the worker returned. Keep a raced sealed
            // producer owned until the connection loop can schedule cleanup.
            pending.insert(stream_id, completion);
        }
        return;
    }

    if let Some(existing) = pending.get_mut(&stream_id) {
        // Retain a late sealed producer even when its raw terminal frame has
        // already been delivered; shutdown still owns bounded producer cleanup.
        // Never append a worker result after any terminal frame was delivered.
        if existing.terminal_delivered {
            transfer_sealed_completion_state(existing, &mut completion);
            existing.release_guards_after_worker_completion();
            return;
        }
        existing.worker_completed = true;
        existing.terminal_claimed |= completion.terminal_claimed;
        let cancelled_before_completion = cancelled.remove(&stream_id);
        if !completion.terminal_claimed && (cancellation_requested || cancelled_before_completion) {
            // Queue cancellation before transferring a raced producer: the
            // queue helper intentionally removes producers from result state,
            // while the cleanup owner below must retain it for bounded drain.
            queue_cancellation_actions(existing, stream_id);
            transfer_sealed_completion_state(existing, &mut completion);
            existing.release_guards_after_worker_completion();
            return;
        }
        transfer_sealed_completion_state(existing, &mut completion);
        existing.release_guards_after_worker_completion();
        existing.actions.extend(completion.actions);
        existing.cancellation = completion.cancellation;
        return;
    }

    if cancellation_requested {
        // Keep a raced completion owned until its bounded producer cleanup can
        // be scheduled by the connection loop instead of dropping it here.
        pending.insert(stream_id, completion);
        return;
    }
    if cancelled.remove(&stream_id) && !completion.terminal_claimed {
        completion.actions = cancellation_actions(stream_id, completion.cancellation.clone());
    }
    pending.insert(stream_id, completion);
}

struct StartedDispatch {
    accepted: ServerAction,
    started: Option<ServerAction>,
    start_gate: Option<oneshot::Sender<()>>,
    future: DispatchFuture,
}

enum InvokePreflight {
    Rejected(CallFailure),
    Accepted(Option<SealedInvocationContinuation>),
}

type InvokePreflightFuture =
    Pin<Box<dyn Future<Output = Result<InvokePreflight, PostgresKernelError>> + Send>>;

struct ResourceDispatchCompletion {
    actions: VecDeque<ResourceServerFrame>,
    producer: Option<AuthenticatedServerResourceProducer>,
    producer_waiting_bytes: Option<u64>,
    terminal_provenance: ResourceTerminalProvenance,
}

struct StartedResourceDispatch {
    future: ResourceDispatchFuture,
    cancellation: ResourceCancellation,
}

struct ResourceTask {
    handle: JoinHandle<()>,
    cancellation: ResourceCancellation,
    active: bool,
    guards: Option<ResourceTaskGuards>,
}

struct ResourceTaskGuards {
    _operation: OwnedSemaphorePermit,
    _payload: Vec<PayloadReservation>,
}

type DispatchFuture = Pin<Box<dyn Future<Output = DispatchCompletion> + Send>>;
type ResourceDispatchFuture = Pin<Box<dyn Future<Output = ResourceDispatchCompletion> + Send>>;

fn redacted_invoke_failure(
    invocation: InvocationId,
    phase: InvocationFailurePhase,
    code: &'static str,
    message: &'static str,
    retryability: InvocationRetryability,
) -> InvocationEventBatch {
    redacted_invoke_failure_at(invocation, 1, 2, phase, code, message, retryability)
}

fn redacted_invoke_failure_at(
    invocation: InvocationId,
    event_sequence: u64,
    outer_sequence: u64,
    phase: InvocationFailurePhase,
    code: &'static str,
    message: &'static str,
    retryability: InvocationRetryability,
) -> InvocationEventBatch {
    let failure = InvocationFailure::new(phase, code, message, None, retryability)
        .expect("checked redacted invocation failure");
    let failed = InvokeEvent::new(
        invocation,
        event_sequence,
        InvocationEventBody::Failed(failure),
    )
    .expect("checked invocation failed event");
    InvocationEventBatch::new(vec![InvocationEventRecord::new(outer_sequence, failed)])
        .expect("checked invocation failure batch")
}

fn without_started_event(events: InvocationEventBatch) -> InvocationEventBatch {
    InvocationEventBatch::new(events.records().iter().skip(1).cloned().collect())
        .expect("sealed invocation completion contains a terminal event")
}

fn sealed_presentation_failure_actions(
    stream: u64,
    invocation: InvocationId,
) -> VecDeque<ServerAction> {
    VecDeque::from([
        ServerAction::InvokeEvents {
            stream,
            events: redacted_invoke_failure(
                invocation,
                InvocationFailurePhase::Internal,
                "INVOKE_INTERNAL_FAILURE",
                "invocation could not complete",
                InvocationRetryability::Unknown,
            ),
        },
        ServerAction::Completed { stream },
    ])
}

#[derive(Clone)]
struct RawDispatchService {
    kernel: PostgresKernel,
    invoke_cancellations: Arc<Mutex<BTreeMap<u64, ResourceCancellation>>>,
    resource_broker: Option<SharedInvokeBroker>,
}

trait DispatchService: Clone + Send + Sync + 'static {
    fn start(&self, session: AuthenticatedSession, stream: u64, call: RawCall) -> StartedDispatch;

    fn preflight_invoke(
        &self,
        _session: AuthenticatedSession,
        _request: orna_protocol::RetainedInvokeRequest,
        _version: RawProtocolVersion,
    ) -> InvokePreflightFuture {
        Box::pin(async { Ok(InvokePreflight::Accepted(None)) })
    }

    fn start_invoke(
        &self,
        _session: AuthenticatedSession,
        stream: u64,
        _request: orna_protocol::RetainedInvokeRequest,
        _version: &RawProtocolVersion,
        _continuation: Option<SealedInvocationContinuation>,
    ) -> StartedDispatch {
        let invocation = InvocationId::new();
        StartedDispatch {
            accepted: ServerAction::Accepted { stream, invocation },
            started: None,
            start_gate: None,
            future: Box::pin(async move {
                DispatchCompletion {
                    sealed_producer: None,
                    sealed_invocation: None,
                    sealed_next_event_sequence: 1,
                    sealed_next_outer_sequence: 2,
                    actions: VecDeque::from([ServerAction::Completed { stream }]),
                    cancellation: ServerAction::InvokeCancelled { stream },
                    cancellation_token: None,
                    start_gate: None,
                    start_delivered: false,
                    terminal_delivered: false,
                    terminal_claimed: false,
                    worker_completed: false,
                    _guards: None,
                }
            }),
        }
    }

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        _request: ResourceRequest,
        _resources: LocalRawSocketResources,
        _version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        None
    }

    /// Authorises a raw resource request before reserving dispatch capacity.
    ///
    /// Test and compatibility dispatchers do not carry durable resource
    /// provenance, so they retain the existing permissive default. The real
    /// raw dispatcher overrides this at the authenticated broker boundary.
    fn authorize_resource_request(&self, _request: &ResourceRequest) -> bool {
        true
    }

    fn record_resource_terminal_provenance(
        &self,
        _stream_id: u64,
        _request_id: InvocationId,
        _provenance: ResourceTerminalProvenance,
    ) {
    }

    fn cancelled(&self, _stream: u64) {}

    fn session_bridge(&self) -> Option<Arc<crate::invoke::SessionBridge>> {
        None
    }
}

fn sealed_result_cancellation_won(
    cancellation: &ResourceCancellation,
    execution: &Result<SealedInvocationExecution, PostgresKernelError>,
) -> bool {
    cancellation.is_requested()
        && matches!(
            execution,
            Ok(SealedInvocationExecution::Result(
                SealedInvocationResult::Completed { .. }
            ))
        )
}

impl DispatchService for RawDispatchService {
    fn session_bridge(&self) -> Option<Arc<crate::invoke::SessionBridge>> {
        self.resource_broker
            .as_ref()
            .and_then(SharedInvokeBroker::session_bridge)
    }

    fn cancelled(&self, stream: u64) {
        if let Some(cancellation) = self
            .invoke_cancellations
            .lock()
            .expect("invocation cancellation lock")
            .get(&stream)
        {
            cancellation.request_cancel();
        }
    }

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
                sealed_producer: None,
                sealed_invocation: None,
                sealed_next_event_sequence: 1,
                sealed_next_outer_sequence: 2,
                actions: result.into_actions().into(),
                cancellation,
                cancellation_token: None,
                start_gate: None,
                start_delivered: false,
                terminal_delivered: false,
                terminal_claimed: false,
                worker_completed: false,
                _guards: None,
            }
        });
        StartedDispatch {
            accepted,
            started: None,
            start_gate: None,
            future,
        }
    }

    fn preflight_invoke(
        &self,
        session: AuthenticatedSession,
        request: orna_protocol::RetainedInvokeRequest,
        _version: RawProtocolVersion,
    ) -> InvokePreflightFuture {
        let kernel = self.kernel.clone();
        Box::pin(async move {
            match kernel
                .validate_sealed_sys_invoke(&session, SEALED_CONNECTION_PROTOCOL_MAJOR, &request)
                .await?
            {
                SealedInvocationPreflight::Rejected { failure } => {
                    Ok(InvokePreflight::Rejected(failure))
                }
                SealedInvocationPreflight::Accepted(continuation) => {
                    Ok(InvokePreflight::Accepted(Some(continuation)))
                }
            }
        })
    }

    fn start_invoke(
        &self,
        _session: AuthenticatedSession,
        stream: u64,
        _request: orna_protocol::RetainedInvokeRequest,
        _version: &RawProtocolVersion,
        continuation: Option<SealedInvocationContinuation>,
    ) -> StartedDispatch {
        let continuation = continuation.expect("sealed invocation preflight continuation");
        let invocation = continuation.invocation();
        if let Some(broker) = &self.resource_broker {
            broker
                .install_session_bridge(invocation, stream)
                .expect("one session bridge per authenticated root invocation");
        }
        let dispatch_session = _session.clone();
        // The worker below uses a short-lived runtime; stream producers must
        // stay owned by this raw-socket driver runtime.
        let resource_runtime = tokio::runtime::Handle::current();
        let kernel = self.kernel.clone();
        let started = ServerAction::InvokeEvents {
            stream,
            events: continuation.started_events().clone(),
        };
        let accepted = ServerAction::Accepted { stream, invocation };
        let (start_gate, start_signal) = oneshot::channel();
        let cancellation = ResourceCancellation::new();
        self.invoke_cancellations
            .lock()
            .expect("invocation cancellation lock")
            .insert(stream, cancellation.clone());
        let cancellation_for_task = cancellation.clone();
        let cancellations = self.invoke_cancellations.clone();
        let resource_broker = self.resource_broker.clone();
        let future = Box::pin(async move {
            let mut operation = match continuation.prepare_sealed_sys_invoke_after_accept().await {
                Ok(operation) => operation,
                Err(source) => {
                    report_private_dispatch_source(&source);
                    cancellations
                        .lock()
                        .expect("invocation cancellation lock")
                        .remove(&stream);
                    return DispatchCompletion {
                        sealed_producer: None,
                        sealed_invocation: Some(invocation),
                        sealed_next_event_sequence: 1,
                        sealed_next_outer_sequence: 2,
                        actions: VecDeque::from([
                            ServerAction::InvokeEvents {
                                stream,
                                events: redacted_invoke_failure(
                                    invocation,
                                    InvocationFailurePhase::Internal,
                                    "INVOKE_INTERNAL_FAILURE",
                                    "invocation could not complete",
                                    InvocationRetryability::Unknown,
                                ),
                            },
                            ServerAction::Completed { stream },
                        ]),
                        cancellation: ServerAction::InvokeCancelled { stream },
                        cancellation_token: Some(cancellation_for_task.clone()),
                        start_gate: None,
                        start_delivered: false,
                        terminal_delivered: false,
                        terminal_claimed: false,
                        worker_completed: false,
                        _guards: None,
                    };
                }
            };
            let _ = tokio::select! {
                biased;
                _ = start_signal => {}
                _ = cancellation_for_task.cancelled() => {}
            };
            let cancellation = cancellation_for_task.clone();
            let worker_kernel = kernel.clone();
            let worker_session = dispatch_session.clone();
            let worker_active = operation.active_revision();
            let execution = tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel",
                        record: "raw_socket".to_string(),
                        rule: "sealed invocation worker runtime must start",
                    })?;
                runtime.block_on(async move {
                    let mut state = ClientStateStore::new();
                    let mut capability_audit_appended = false;
                    let mut resource_executor = match resource_broker {
                        Some(broker) => InstalledClientResourceExecutor::new_with_broker(
                            worker_kernel,
                            worker_session,
                            worker_active,
                            broker,
                            cancellation.clone(),
                        ),
                        None => InstalledClientResourceExecutor::new(
                            worker_kernel,
                            worker_session,
                            worker_active,
                        ),
                    };
                    operation
                        .execute_after_started(
                            Some(&mut resource_executor),
                            &mut state,
                            &mut capability_audit_appended,
                            &cancellation,
                            resource_runtime,
                        )
                        .await
                })
            })
            .await
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel",
                record: "raw_socket".to_string(),
                rule: "sealed invocation worker must not panic",
            })
            .and_then(|result| result);
            let cancellation_won_after_execution =
                sealed_result_cancellation_won(&cancellation_for_task, &execution);
            let (actions, sealed_producer) = match execution {
                Ok(SealedInvocationExecution::ServerStream(producer)) => {
                    (VecDeque::new(), Some(producer))
                }
                Ok(SealedInvocationExecution::Result(_)) if cancellation_won_after_execution => (
                    cancellation_actions(stream, ServerAction::InvokeCancelled { stream }),
                    None,
                ),
                Ok(SealedInvocationExecution::Result(SealedInvocationResult::Completed {
                    events,
                    ..
                }))
                | Ok(SealedInvocationExecution::Result(SealedInvocationResult::Failed {
                    events,
                    ..
                })) => (
                    VecDeque::from([
                        ServerAction::InvokeEvents {
                            stream,
                            events: without_started_event(events),
                        },
                        ServerAction::Completed { stream },
                    ]),
                    None,
                ),
                Ok(SealedInvocationExecution::Result(SealedInvocationResult::Denied {
                    ..
                })) => (
                    VecDeque::from([
                        ServerAction::InvokeEvents {
                            stream,
                            events: redacted_invoke_failure(
                                invocation,
                                InvocationFailurePhase::Authorise,
                                "INVOKE_DENIED",
                                "invocation was not permitted",
                                InvocationRetryability::No,
                            ),
                        },
                        ServerAction::Completed { stream },
                    ]),
                    None,
                ),
                Ok(SealedInvocationExecution::Result(
                    SealedInvocationResult::PresentationFailed { .. },
                )) => (
                    sealed_presentation_failure_actions(stream, invocation),
                    None,
                ),
                Ok(SealedInvocationExecution::Cancelled { .. }) => (
                    cancellation_actions(stream, ServerAction::InvokeCancelled { stream }),
                    None,
                ),
                Err(source) => {
                    report_private_dispatch_source(&source);
                    (
                        VecDeque::from([
                            ServerAction::InvokeEvents {
                                stream,
                                events: redacted_invoke_failure(
                                    invocation,
                                    InvocationFailurePhase::Internal,
                                    "INVOKE_INTERNAL_FAILURE",
                                    "invocation could not complete",
                                    InvocationRetryability::Unknown,
                                ),
                            },
                            ServerAction::Completed { stream },
                        ]),
                        None,
                    )
                }
            };
            cancellations
                .lock()
                .expect("invocation cancellation lock")
                .remove(&stream);
            DispatchCompletion {
                actions,
                cancellation: ServerAction::InvokeCancelled { stream },
                cancellation_token: Some(cancellation_for_task),
                sealed_producer,
                sealed_invocation: Some(invocation),
                sealed_next_event_sequence: 1,
                sealed_next_outer_sequence: 2,
                start_gate: None,
                start_delivered: false,
                terminal_delivered: false,
                terminal_claimed: false,
                worker_completed: false,
                _guards: None,
            }
        });
        StartedDispatch {
            accepted,
            started: Some(started),
            start_gate: Some(start_gate),
            future,
        }
    }

    fn authorize_resource_request(&self, request: &ResourceRequest) -> bool {
        self.resource_broker
            .as_ref()
            .is_some_and(|broker| broker.take_expected_resource_request(request))
    }

    fn record_resource_terminal_provenance(
        &self,
        stream_id: u64,
        request_id: InvocationId,
        provenance: ResourceTerminalProvenance,
    ) {
        if let Some(broker) = self.resource_broker.as_ref() {
            broker.record_resource_terminal_provenance(stream_id, request_id, provenance);
        }
    }

    fn start_resource(
        &self,
        session: AuthenticatedSession,
        request: ResourceRequest,
        _resources: LocalRawSocketResources,
        _version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        let kernel = self.kernel.clone();
        let cancellation = ResourceCancellation::new();
        let operation_cancellation = cancellation.clone();
        let future = Box::pin(async move {
            match kernel
                .start_authenticated_server_resource_producer(
                    &session,
                    &request,
                    &operation_cancellation,
                )
                .await
            {
                Ok(AuthenticatedServerResourceStart::Accepted(producer)) => {
                    let accepted = producer.accepted();
                    ResourceDispatchCompletion {
                        actions: VecDeque::from([resource_accepted_frame(accepted)]),
                        producer: Some(producer),
                        producer_waiting_bytes: None,
                        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                    }
                }
                Ok(AuthenticatedServerResourceStart::Failed {
                    stream_id,
                    request_id,
                    failure,
                }) => ResourceDispatchCompletion {
                    actions: VecDeque::from([ResourceServerFrame::Failed(
                        orna_protocol::ResourceFailed {
                            stream_id,
                            request_id,
                            target_revision: request.target_revision,
                            failure,
                        },
                    )]),
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                },
                Err(error) => {
                    report_private_dispatch_source(&error);
                    ResourceDispatchCompletion {
                        actions: resource_internal_failure(&request),
                        producer: None,
                        producer_waiting_bytes: None,
                        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                    }
                }
            }
        });
        Some(StartedResourceDispatch {
            future,
            cancellation,
        })
    }
}

#[cfg(test)]
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
            target_revision: request.target_revision,
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
                    target_revision,
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
                    target_revision,
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

async fn shutdown_resource_producer(producer: AuthenticatedServerResourceProducer) {
    producer.cancel();
    let _ = timeout(
        RESOURCE_PRODUCER_SHUTDOWN_TIMEOUT,
        producer.pull(ResourceCredit {
            item_count: 0,
            byte_count: 0,
        }),
    )
    .await;
}

/// Completes a started sealed producer after peer loss without retaining any
/// undeliverable Event. Each pull admits one bounded value and immediately
/// discards it; the producer owns the durable terminal commit.
async fn drain_sealed_producer(producer: AuthenticatedServerResourceProducer) {
    let mut byte_credit = 1024 * 1024 * 1024;
    loop {
        let Some(credit) = ResourceCredit::new(1, byte_credit) else {
            return;
        };
        match producer.pull(credit).await {
            Ok(AuthenticatedServerResourceEvent::Values { .. }) => {}
            Ok(AuthenticatedServerResourceEvent::Waiting { required_bytes }) => {
                byte_credit = required_bytes.min(1024 * 1024 * 1024).max(1);
            }
            Ok(
                AuthenticatedServerResourceEvent::Completed { .. }
                | AuthenticatedServerResourceEvent::Failed { .. }
                | AuthenticatedServerResourceEvent::Cancelled,
            )
            | Err(_) => return,
        }
    }
}

fn schedule_shutdown_task<F>(shutdown_tasks: &mut JoinSet<()>, shutdown: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    shutdown_tasks.spawn(shutdown);
}

async fn retain_shutdown_tasks_until_terminal(mut shutdown_tasks: JoinSet<()>) {
    while let Some(joined) = shutdown_tasks.join_next().await {
        let _ = joined;
    }
}

fn schedule_resource_producer_shutdown(
    producer: AuthenticatedServerResourceProducer,
    shutdown_tasks: &mut JoinSet<()>,
    guards: Option<ResourceTaskGuards>,
) {
    schedule_shutdown_task(shutdown_tasks, async move {
        shutdown_resource_producer(producer).await;
        drop(guards);
    });
}

fn schedule_pending_sealed_cleanups(
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    shutdown_tasks: &mut JoinSet<()>,
) {
    for completion in pending.values_mut() {
        let cancellation_requested = completion
            .cancellation_token
            .as_ref()
            .is_some_and(ResourceCancellation::is_requested);
        if cancellation_requested {
            schedule_sealed_completion_shutdown(completion, shutdown_tasks);
        }
    }
}

fn schedule_sealed_completion_shutdown(
    completion: &mut DispatchCompletion,
    shutdown_tasks: &mut JoinSet<()>,
) {
    let Some(producer) = completion.sealed_producer.take() else {
        return;
    };
    let guards = completion._guards.take();
    schedule_shutdown_task(shutdown_tasks, async move {
        shutdown_resource_producer(producer).await;
        drop(guards);
    });
}

fn should_drain_sealed_on_disconnect(completion: &DispatchCompletion) -> bool {
    completion.sealed_invocation.is_some()
        && completion.sealed_producer.is_some()
        && completion.start_delivered
        && !completion
            .cancellation_token
            .as_ref()
            .is_some_and(ResourceCancellation::is_requested)
}

fn schedule_sealed_completion_drain(
    completion: &mut DispatchCompletion,
    shutdown_tasks: &mut JoinSet<()>,
) {
    let Some(producer) = completion.sealed_producer.take() else {
        return;
    };
    let guards = completion._guards.take();
    schedule_shutdown_task(shutdown_tasks, async move {
        drain_sealed_producer(producer).await;
        drop(guards);
    });
}

fn resource_accepted_frame(accepted: AuthenticatedServerResourceAccepted) -> ResourceServerFrame {
    ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
        stream_id: accepted.stream_id,
        request_id: accepted.request_id,
        nested_invocation_id: accepted.nested_invocation_id,
        target_revision: accepted.target_revision,
        resource_kind: match accepted.resource_kind {
            AuthenticatedServerResourceKind::Single => ResourceKind::Single,
            AuthenticatedServerResourceKind::Stream => ResourceKind::Stream,
        },
    })
}

fn resource_event_completion(
    accepted: AuthenticatedServerResourceAccepted,
    producer: AuthenticatedServerResourceProducer,
    event: Result<AuthenticatedServerResourceEvent, PostgresKernelError>,
) -> ResourceDispatchCompletion {
    let failure = |producer| ResourceDispatchCompletion {
        actions: VecDeque::from([ResourceServerFrame::Failed(orna_protocol::ResourceFailed {
            stream_id: accepted.stream_id,
            request_id: accepted.request_id,
            target_revision: accepted.target_revision,
            failure: CallFailure::InternalFailure,
        })]),
        producer: Some(producer),
        producer_waiting_bytes: None,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
    };
    match event {
        Ok(AuthenticatedServerResourceEvent::Values {
            batch_sequence,
            item_count,
            byte_count,
            values,
        }) => {
            let Ok(item_count) = u32::try_from(item_count) else {
                return failure(producer);
            };
            let Ok(byte_count) = u32::try_from(byte_count) else {
                return failure(producer);
            };
            if item_count == 0 || item_count as usize != values.len() {
                return failure(producer);
            }
            ResourceDispatchCompletion {
                actions: VecDeque::from([ResourceServerFrame::Values(
                    orna_protocol::ResourceValues {
                        stream_id: accepted.stream_id,
                        request_id: accepted.request_id,
                        target_revision: accepted.target_revision,
                        batch_sequence,
                        item_count,
                        byte_count,
                        values,
                    },
                )]),
                producer: Some(producer),
                producer_waiting_bytes: None,
                terminal_provenance: ResourceTerminalProvenance::Uncommitted,
            }
        }
        Ok(AuthenticatedServerResourceEvent::Completed {
            final_batch_sequence,
            total_items,
            total_bytes: _,
        }) => ResourceDispatchCompletion {
            actions: VecDeque::from([ResourceServerFrame::Completed(
                orna_protocol::ResourceCompleted {
                    stream_id: accepted.stream_id,
                    request_id: accepted.request_id,
                    target_revision: accepted.target_revision,
                    final_batch_sequence,
                    total_items,
                },
            )]),
            producer: None,
            producer_waiting_bytes: None,
            terminal_provenance: ResourceTerminalProvenance::Authenticated,
        },
        Ok(AuthenticatedServerResourceEvent::Failed { failure: reason }) => {
            ResourceDispatchCompletion {
                actions: VecDeque::from([ResourceServerFrame::Failed(
                    orna_protocol::ResourceFailed {
                        stream_id: accepted.stream_id,
                        request_id: accepted.request_id,
                        target_revision: accepted.target_revision,
                        failure: reason,
                    },
                )]),
                producer: None,
                producer_waiting_bytes: None,
                terminal_provenance: ResourceTerminalProvenance::Authenticated,
            }
        }
        Ok(AuthenticatedServerResourceEvent::Cancelled) => ResourceDispatchCompletion {
            actions: VecDeque::from([ResourceServerFrame::Cancelled(
                orna_protocol::ResourceCancelled {
                    stream_id: accepted.stream_id,
                    request_id: accepted.request_id,
                    target_revision: accepted.target_revision,
                    reason: orna_protocol::ResourceCancellationCode::ServerRequested,
                },
            )]),
            producer: None,
            producer_waiting_bytes: None,
            terminal_provenance: ResourceTerminalProvenance::Authenticated,
        },
        Ok(AuthenticatedServerResourceEvent::Waiting { required_bytes }) => {
            ResourceDispatchCompletion {
                actions: VecDeque::new(),
                producer: Some(producer),
                producer_waiting_bytes: Some(required_bytes),
                terminal_provenance: ResourceTerminalProvenance::Uncommitted,
            }
        }
        Err(error) => {
            report_private_dispatch_source(&error);
            failure(producer)
        }
    }
}

fn schedule_resource_pulls(
    connection: &ResourceProtocolConnection,
    pending: &mut BTreeMap<u64, ResourceDispatchCompletion>,
    tasks: &mut BTreeMap<u64, ResourceTask>,
    sender: &mpsc::Sender<(u64, ResourceDispatchCompletion)>,
) {
    let stream_ids: Vec<_> = pending.keys().copied().collect();
    for stream_id in stream_ids {
        let Some(completion) = pending.get(&stream_id) else {
            continue;
        };
        if !completion.actions.is_empty() || !tasks.get(&stream_id).is_some_and(|task| !task.active)
        {
            continue;
        }
        let Some(producer) = completion.producer.as_ref() else {
            continue;
        };
        let accepted = producer.accepted();
        let Ok(credit) = connection.resource_credit(accepted.stream_id, accepted.request_id) else {
            continue;
        };
        if completion
            .producer_waiting_bytes
            .is_some_and(|required| credit.byte_available < required)
        {
            continue;
        }
        if completion.producer_waiting_bytes.is_some() && credit.item_available == 0 {
            continue;
        }
        let Some(credit) =
            resource_producer_credit(accepted, credit.item_available, credit.byte_available)
        else {
            continue;
        };
        let producer = pending
            .remove(&stream_id)
            .and_then(|completion| completion.producer)
            .expect("pending producer exists");
        let task = tasks.get_mut(&stream_id).expect("producer task exists");
        let cancellation = task.cancellation.clone();
        let accepted = producer.accepted();
        let sender = sender.clone();
        let handle = tokio::spawn(async move {
            let event = producer.pull(credit).await;
            let mut completion = resource_event_completion(accepted, producer, event);
            if cancellation.is_requested()
                || completion.actions.iter().any(resource_action_is_terminal)
            {
                if let Some(producer) = completion.producer.take() {
                    shutdown_resource_producer(producer).await;
                }
            }
            if cancellation.is_requested() {
                completion = ResourceDispatchCompletion {
                    actions: VecDeque::new(),
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                };
            }
            let _ = sender.send((stream_id, completion)).await;
        });
        task.handle = handle;
        task.active = true;
    }
}

/// Builds one bounded producer pull from the connection's available credit.
///
/// A producer pull never grants more than one value. When a live resource has
/// exhausted either client window, the raw scheduler may issue an internal
/// terminal-only probe; producers must not emit a value from that probe.
fn resource_producer_credit(
    accepted: AuthenticatedServerResourceAccepted,
    item_available: u64,
    byte_available: u64,
) -> Option<ResourceCredit> {
    if matches!(
        accepted.resource_kind,
        AuthenticatedServerResourceKind::Stream
    ) && (item_available == 0 || byte_available == 0)
    {
        return Some(ResourceCredit {
            item_count: item_available.min(1),
            byte_count: byte_available,
        });
    }
    if matches!(
        accepted.resource_kind,
        AuthenticatedServerResourceKind::Single
    ) && (item_available == 0 || byte_available == 0)
    {
        // A scalar terminal pull is an internal EOF probe. It carries no
        // value credit when either client window is exhausted; the producer
        // accepts it only after the scalar value has already been delivered.
        return Some(ResourceCredit {
            item_count: item_available.min(1),
            byte_count: byte_available,
        });
    }
    ResourceCredit::new(item_available.min(1), byte_available)
}

#[cfg(test)]
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

fn resource_action_stream_id(action: &ResourceServerFrame) -> u64 {
    match action {
        ResourceServerFrame::Accepted(frame) => frame.stream_id,
        ResourceServerFrame::Values(frame) => frame.stream_id,
        ResourceServerFrame::Completed(frame) => frame.stream_id,
        ResourceServerFrame::Failed(frame) => frame.stream_id,
        ResourceServerFrame::Cancelled(frame) => frame.stream_id,
    }
}

fn pending_resource_error_is_local(
    request: &ResourceRequest,
    action: &ResourceServerFrame,
    source: &ResourceConnectionError,
) -> bool {
    resource_action_stream_id(action) == request.stream_id
        && !matches!(
            source,
            ResourceConnectionError::InsufficientCredit { .. }
                | ResourceConnectionError::UnknownStream { .. }
                | ResourceConnectionError::StreamNotIncreasing { .. }
                | ResourceConnectionError::TooManyLiveResources
                | ResourceConnectionError::RequestIdHistoryExhausted
        )
}

fn resource_internal_failure(request: &ResourceRequest) -> VecDeque<ResourceServerFrame> {
    VecDeque::from([ResourceServerFrame::Failed(orna_protocol::ResourceFailed {
        stream_id: request.stream_id,
        request_id: request.request_id,
        target_revision: request.target_revision,
        failure: CallFailure::InternalFailure,
    })])
}

struct UnstartedDispatch {
    stream: u64,
    future: DispatchFuture,
    guards: Option<DispatchGuards>,
    defer_once: bool,
}

struct InvokePreflightCompletion {
    result: Result<InvokePreflight, PostgresKernelError>,
    session: AuthenticatedSession,
    request: orna_protocol::RetainedInvokeRequest,
    guards: DispatchGuards,
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
    let (frame_sender, mut frame_receiver) = mpsc::channel(FRAME_CHANNEL_CAPACITY);
    let resource_read_state = ResourceReadState::default();
    let reader_task = spawn_frame_reader(
        reader,
        version.clone(),
        resources.clone(),
        resource_read_state.clone(),
        frame_sender,
    );
    let (resource_completion_sender, mut resource_completion_receiver) =
        mpsc::channel::<(u64, ResourceDispatchCompletion)>(RESOURCE_COMPLETION_CHANNEL_CAPACITY);
    let mut connection = ProtocolConnection::new();
    let mut resource_connection = ResourceProtocolConnection::new();
    let mut retained_payload = BTreeMap::<u64, Vec<PayloadReservation>>::new();
    let mut cancelled = BTreeSet::<u64>::new();
    let mut cancelled_pending = BTreeSet::<u64>::new();
    let mut preflight_pending = BTreeSet::<u64>::new();
    let mut preflight_cancelled = BTreeSet::<u64>::new();
    let mut pending = BTreeMap::<u64, DispatchCompletion>::new();
    let mut resource_cancelled =
        BTreeMap::<u64, (orna_protocol::ResourceCancel, RevisionPair)>::new();
    let mut resource_pending = BTreeMap::<u64, ResourceDispatchCompletion>::new();
    let mut resource_tasks = BTreeMap::<u64, ResourceTask>::new();
    let mut resource_requests = BTreeMap::<u64, ResourceRequest>::new();
    let mut tasks = JoinSet::<(u64, DispatchCompletion)>::new();
    let mut preflight_tasks = JoinSet::<(u64, InvokePreflightCompletion)>::new();
    let mut buffered_frames = VecDeque::<Result<Option<IncomingFrame>, LocalRawSocketError>>::new();
    let mut preflight_frame_streak = 0_usize;
    // Cancellation owns producer shutdown work on this connection set; the event
    // loop never awaits it inline.
    let mut producer_shutdown = JoinSet::<()>::new();
    let mut sealed_pull_tasks = JoinSet::<SealedPullTaskResult>::new();
    let mut sealed_pull_in_flight = BTreeSet::<u64>::new();
    let mut sealed_pull_waiting_bytes = BTreeMap::<u64, u64>::new();
    let mut unstarted = VecDeque::<UnstartedDispatch>::new();
    let result = loop {
        let mut producer_shutdown_error = None;
        for _ in 0..CONNECTION_LIMIT {
            match producer_shutdown.try_join_next() {
                Some(Ok(())) => {}
                Some(Err(source)) => {
                    producer_shutdown_error = Some(source);
                    break;
                }
                None => break,
            }
        }
        if let Some(source) = producer_shutdown_error {
            break Err(LocalRawSocketError::DispatchTask { source });
        }
        schedule_pending_sealed_cleanups(&mut pending, &mut producer_shutdown);

        match flush_resource_pending(
            &version,
            &mut resource_connection,
            &mut resource_pending,
            &mut resource_requests,
            &mut resource_tasks,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => break Ok(()),
            Err(error) => break Err(error),
        }
        resource_read_state.set_active(resource_connection.live_resources() != 0);
        schedule_resource_pulls(
            &resource_connection,
            &mut resource_pending,
            &mut resource_tasks,
            &resource_completion_sender,
        );
        let mut pending_flush_fairness_yielded = false;
        match flush_pending_with_fairness_boundary(
            &version,
            &mut connection,
            &mut pending,
            &mut sealed_pull_tasks,
            &mut sealed_pull_in_flight,
            &mut sealed_pull_waiting_bytes,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
            &mut pending_flush_fairness_yielded,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => break Ok(()),
            Err(error) => break Err(error),
        }
        match flush_session_pending(&dispatcher, &mut writer, &mut shutdown).await {
            Ok(true) => {}
            Ok(false) => break Ok(()),
            Err(error) => break Err(error),
        }

        enum Next {
            Frame(Result<Option<IncomingFrame>, LocalRawSocketError>),
            Completion(Option<Result<(u64, DispatchCompletion), JoinError>>),
            Preflight(Option<Result<(u64, InvokePreflightCompletion), JoinError>>),
            ResourceCompletion(Option<(u64, ResourceDispatchCompletion)>),
            SealedPull(Option<Result<SealedPullTaskResult, JoinError>>),
            Shutdown,
            Start,
            SessionWake,
        }

        if *shutdown.borrow() {
            break Ok(());
        }
        let preflight_cancel_queued = if preflight_tasks.is_empty() {
            false
        } else {
            // Snapshot a bounded reader queue before allowing a ready preflight
            // to win a fairness turn. This preserves FIFO cancellation precedence
            // for every CALL_CANCEL already decoded by the frame reader.
            for _ in buffered_frames.len()..FRAME_CHANNEL_CAPACITY {
                match frame_receiver.try_recv() {
                    Ok(frame) => buffered_frames.push_back(frame),
                    Err(mpsc::error::TryRecvError::Empty)
                    | Err(mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
            buffered_frames.iter().any(|frame| match frame {
                Ok(Some(IncomingFrame::Raw(raw))) => match &raw.frame {
                    ClientFrame::CallCancel { stream } => preflight_pending.contains(stream),
                    _ => false,
                },
                _ => false,
            })
        };
        let fairness_turn = !preflight_tasks.is_empty()
            && preflight_frame_streak >= PREFLIGHT_FRAME_FAIRNESS_BUDGET
            && !preflight_cancel_queued;
        let next = if pending_flush_fairness_yielded {
            if let Some(frame) = buffered_frames.pop_front() {
                Next::Frame(frame)
            } else {
                tokio::select! {
                    biased;
                    _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                    _ = wait_for_session_outbound(dispatcher.session_bridge()) => Next::SessionWake,
                    frame = frame_receiver.recv() => Next::Frame(frame.unwrap_or(Ok(None))),
                    preflight = preflight_tasks.join_next(), if !preflight_tasks.is_empty() => {
                        Next::Preflight(preflight)
                    }
                    completion = tasks.join_next(), if !tasks.is_empty() => {
                        Next::Completion(completion)
                    }
                    resource = resource_completion_receiver.recv() => Next::ResourceCompletion(resource),
                    sealed = sealed_pull_tasks.join_next(), if !sealed_pull_tasks.is_empty() => {
                        Next::SealedPull(sealed)
                    }
                    () = tokio::task::yield_now(), if !unstarted.is_empty() => Next::Start,
                }
            }
        } else if let Some(dispatch) = unstarted.front_mut() {
            if dispatch.defer_once {
                dispatch.defer_once = false;
                if let Some(frame) = buffered_frames.pop_front() {
                    Next::Frame(frame)
                } else {
                    tokio::select! {
                        biased;
                        _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                        _ = wait_for_session_outbound(dispatcher.session_bridge()) => Next::SessionWake,
                        frame = frame_receiver.recv() => Next::Frame(frame.unwrap_or(Ok(None))),
                        preflight = preflight_tasks.join_next(), if !preflight_tasks.is_empty() => {
                            Next::Preflight(preflight)
                        }
                        resource = resource_completion_receiver.recv() => Next::ResourceCompletion(resource),
                        sealed = sealed_pull_tasks.join_next(), if !sealed_pull_tasks.is_empty() => {
                            Next::SealedPull(sealed)
                        }
                        () = tokio::task::yield_now() => Next::Start,
                    }
                }
            } else {
                Next::Start
            }
        } else if fairness_turn {
            // Once a bounded burst of ordinary frames has been serviced, a
            // preflight completion gets a turn. The queue snapshot above ensures
            // an already-decoded cancellation still takes the frame path first.
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                _ = wait_for_session_outbound(dispatcher.session_bridge()) => Next::SessionWake,
                preflight = preflight_tasks.join_next() => Next::Preflight(preflight),
                completion = tasks.join_next(), if !tasks.is_empty() => {
                    Next::Completion(completion)
                }
                frame = frame_receiver.recv() => Next::Frame(frame.unwrap_or(Ok(None))),
                resource = resource_completion_receiver.recv() => Next::ResourceCompletion(resource),
                sealed = sealed_pull_tasks.join_next(), if !sealed_pull_tasks.is_empty() => {
                    Next::SealedPull(sealed)
                }
            }
        } else if let Some(frame) = buffered_frames.pop_front() {
            Next::Frame(frame)
        } else if tasks.is_empty() && preflight_tasks.is_empty() {
            tokio::select! {
                _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                _ = wait_for_session_outbound(dispatcher.session_bridge()) => Next::SessionWake,
                frame = frame_receiver.recv() => Next::Frame(frame.unwrap_or(Ok(None))),
                resource = resource_completion_receiver.recv() => Next::ResourceCompletion(resource),
                sealed = sealed_pull_tasks.join_next(), if !sealed_pull_tasks.is_empty() => {
                    Next::SealedPull(sealed)
                }
            }
        } else if !preflight_tasks.is_empty() {
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                _ = wait_for_session_outbound(dispatcher.session_bridge()) => Next::SessionWake,
                // A cancellation already decoded by the reader wins over a
                // preflight that became ready in the same scheduler turn.
                frame = frame_receiver.recv() => {
                    Next::Frame(frame.unwrap_or(Ok(None)))
                }
                preflight = preflight_tasks.join_next() => Next::Preflight(preflight),
                completion = tasks.join_next(), if !tasks.is_empty() => {
                    Next::Completion(completion)
                }
                resource = resource_completion_receiver.recv() => Next::ResourceCompletion(resource),
                sealed = sealed_pull_tasks.join_next(), if !sealed_pull_tasks.is_empty() => {
                    Next::SealedPull(sealed)
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => Next::Shutdown,
                _ = wait_for_session_outbound(dispatcher.session_bridge()) => Next::SessionWake,
                completion = tasks.join_next(), if !tasks.is_empty() => {
                    Next::Completion(completion)
                }
                frame = frame_receiver.recv() => {
                    Next::Frame(frame.unwrap_or(Ok(None)))
                }
                resource = resource_completion_receiver.recv() => Next::ResourceCompletion(resource),
                sealed = sealed_pull_tasks.join_next(), if !sealed_pull_tasks.is_empty() => {
                    Next::SealedPull(sealed)
                }
            }
        };

        match next {
            Next::Frame(Ok(Some(incoming))) => {
                if preflight_tasks.is_empty() {
                    preflight_frame_streak = 0;
                } else {
                    preflight_frame_streak = preflight_frame_streak.saturating_add(1);
                }
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
                            &mut cancelled_pending,
                            &mut preflight_pending,
                            &mut preflight_cancelled,
                            &mut preflight_tasks,
                            &mut producer_shutdown,
                            &mut pending,
                            &mut unstarted,
                            &mut writer,
                            &mut shutdown,
                        )
                        .await
                    }
                    IncomingFrame::Session { frame, reservation } => {
                        drop(reservation);
                        match dispatcher.session_bridge() {
                            Some(bridge) => bridge
                                .accept_response(frame)
                                .map_err(|source| LocalRawSocketError::SessionState { source })
                                .map(|()| true),
                            None => Err(LocalRawSocketError::SessionState {
                                source: orna_protocol::SessionStateError::WrongState,
                            }),
                        }
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
                            &mut producer_shutdown,
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
            Next::Preflight(Some(Ok((stream_id, completion)))) => {
                preflight_frame_streak = 0;
                preflight_pending.remove(&stream_id);
                let cancelled_before_accept = preflight_cancelled.remove(&stream_id);
                match finish_invoke_preflight(
                    stream_id,
                    completion,
                    cancelled_before_accept,
                    &dispatcher,
                    &mut pending,
                    &mut unstarted,
                    &mut connection,
                    &version,
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
            Next::Preflight(Some(Err(source))) => {
                break Err(LocalRawSocketError::DispatchTask { source });
            }
            Next::Preflight(None) => {
                preflight_frame_streak = 0;
            }
            Next::Completion(Some(Ok((stream_id, completion)))) => {
                merge_dispatch_completion(
                    stream_id,
                    completion,
                    &mut pending,
                    &mut cancelled,
                    &mut cancelled_pending,
                );
            }
            Next::Completion(Some(Err(source))) => {
                break Err(LocalRawSocketError::DispatchTask { source });
            }
            Next::Completion(None) => {}
            Next::ResourceCompletion(Some((stream_id, completion))) => {
                if let Some(task) = resource_tasks.get_mut(&stream_id) {
                    task.active = false;
                }
                if let Some(request_id) = completion.actions.front().map(resource_action_request_id)
                {
                    dispatcher.record_resource_terminal_provenance(
                        stream_id,
                        request_id,
                        completion.terminal_provenance,
                    );
                }
                store_resource_completion(
                    stream_id,
                    completion,
                    &mut resource_pending,
                    &mut resource_cancelled,
                );
            }
            Next::ResourceCompletion(None) => {}
            Next::SealedPull(Some(result)) => {
                if let Err(error) = merge_sealed_pull_result(
                    result,
                    &mut pending,
                    &mut sealed_pull_in_flight,
                    &mut sealed_pull_waiting_bytes,
                    &mut producer_shutdown,
                ) {
                    break Err(error);
                }
            }
            Next::SealedPull(None) => {}
            Next::Shutdown => break Ok(()),
            Next::SessionWake => {}
            Next::Start => {
                start_one_dispatch(&mut unstarted, &mut tasks);
            }
        }
    };

    reader_task.abort();
    let _ = reader_task.await;
    if let Some(bridge) = dispatcher.session_bridge() {
        bridge.close();
    }
    // Close the socket before waiting for accepted dispatches. A started
    // spawn_blocking worker cannot be aborted safely, so the connection must
    // not remain open while its completion boundary is drained below.
    let _ = writer.shutdown().await;
    drop(writer);
    // Disconnect is an implicit cancellation boundary only before a sealed
    // invocation start Event is delivered. Started sealed workers stay owned
    // by this connection until their durable terminal result is produced.
    let unstarted_streams: BTreeSet<_> = unstarted.iter().map(|dispatch| dispatch.stream).collect();
    for dispatch in &unstarted {
        dispatcher.cancelled(dispatch.stream);
    }
    for (stream_id, completion) in &pending {
        if !unstarted_streams.contains(stream_id) && should_cancel_on_disconnect(completion) {
            dispatcher.cancelled(*stream_id);
        }
    }
    for stream_id in preflight_pending.iter().copied() {
        dispatcher.cancelled(stream_id);
    }
    while !unstarted.is_empty() {
        start_one_dispatch(&mut unstarted, &mut tasks);
    }
    let mut drain_failure = None;
    let preflight_shutdown_completed = timeout(DISPATCH_SHUTDOWN_TIMEOUT, async {
        while let Some(completion) = preflight_tasks.join_next().await {
            if let Err(source) = completion {
                drain_failure.get_or_insert(LocalRawSocketError::DispatchTask { source });
            }
        }
    })
    .await
    .is_ok();
    if !preflight_shutdown_completed {
        preflight_tasks.abort_all();
        while let Some(completion) = preflight_tasks.join_next().await {
            if let Err(source) = completion {
                drain_failure.get_or_insert(LocalRawSocketError::DispatchTask { source });
            }
        }
    }
    for task in resource_tasks.values() {
        task.cancellation.request_cancel();
    }
    resource_connection.shutdown();
    // Late completions can outnumber currently live resources after client
    // cancellation frees a stream slot. Drain them while the dispatch tasks
    // join so a bounded completion channel cannot block shutdown.
    let mut resource_shutdown = JoinSet::new();
    let mut resource_abort_handles = Vec::with_capacity(resource_tasks.len());
    let mut resource_guards = Vec::with_capacity(resource_tasks.len());
    for (_, task) in std::mem::take(&mut resource_tasks) {
        let ResourceTask {
            handle,
            cancellation: _,
            active: _,
            guards,
        } = task;
        if let Some(guards) = guards {
            resource_guards.push(guards);
        }
        resource_abort_handles.push(handle.abort_handle());
        resource_shutdown.spawn(async move {
            let _ = handle.await;
        });
    }
    let resource_shutdown_completed = timeout(RESOURCE_PRODUCER_SHUTDOWN_TIMEOUT, async {
        while !resource_shutdown.is_empty() {
            tokio::select! {
                joined = resource_shutdown.join_next() => {
                    let _ = joined;
                }
                completion = resource_completion_receiver.recv() => {
                    let Some((stream_id, completion)) = completion else {
                        continue;
                    };
                    if completion.producer.is_some() {
                        if let Some(producer) = completion.producer.as_ref() {
                            producer.cancel();
                        }
                        resource_pending.insert(stream_id, completion);
                    }
                }
            }
        }
    })
    .await
    .is_ok();
    if !resource_shutdown_completed {
        for abort_handle in resource_abort_handles {
            abort_handle.abort();
        }
        resource_shutdown.abort_all();
        let _ = timeout(RESOURCE_PRODUCER_SHUTDOWN_TIMEOUT, async {
            while let Some(joined) = resource_shutdown.join_next().await {
                let _ = joined;
            }
        })
        .await;
    }
    drain_resource_completions(
        &dispatcher,
        &mut resource_completion_receiver,
        &mut resource_pending,
        &mut resource_cancelled,
        &mut resource_tasks,
    );
    // An accepted producer can be retained in a credit-starved pending
    // completion after its dispatch task has already finished. Cancel it
    // explicitly before dropping the connection so its transaction releases
    // deterministically rather than relying on sender drop during unwind.
    for completion in resource_pending.values_mut() {
        if let Some(producer) = completion.producer.take() {
            schedule_resource_producer_shutdown(producer, &mut producer_shutdown, None);
        }
    }
    // Every scheduled cleanup is joined before the connection returns; timeout
    // then aborts and drains it.
    let producer_shutdown_completed = timeout(RESOURCE_PRODUCER_SHUTDOWN_TIMEOUT, async {
        while let Some(joined) = producer_shutdown.join_next().await {
            let _ = joined;
        }
    })
    .await
    .is_ok();
    if !producer_shutdown_completed {
        producer_shutdown.abort_all();
        let _ = timeout(RESOURCE_PRODUCER_SHUTDOWN_TIMEOUT, async {
            while let Some(joined) = producer_shutdown.join_next().await {
                let _ = joined;
            }
        })
        .await;
    }
    resource_pending.clear();
    drop(resource_guards);
    let dispatch_shutdown_completed = timeout(DISPATCH_SHUTDOWN_TIMEOUT, async {
        while let Some(completion) = tasks.join_next().await {
            match completion {
                Ok((stream_id, completion)) => merge_dispatch_completion(
                    stream_id,
                    completion,
                    &mut pending,
                    &mut cancelled,
                    &mut cancelled_pending,
                ),
                Err(source) => {
                    drain_failure.get_or_insert(LocalRawSocketError::DispatchTask { source });
                }
            }
        }
    })
    .await
    .is_ok();
    if !dispatch_shutdown_completed {
        // A started spawn_blocking worker cannot be aborted: aborting its
        // dispatch task would detach the worker while it may still execute
        // kernel work. Keep owning the JoinSet and drain it to completion.
        while let Some(completion) = tasks.join_next().await {
            match completion {
                Ok((stream_id, completion)) => merge_dispatch_completion(
                    stream_id,
                    completion,
                    &mut pending,
                    &mut cancelled,
                    &mut cancelled_pending,
                ),
                Err(source) => {
                    drain_failure.get_or_insert(LocalRawSocketError::DispatchTask { source });
                }
            }
        }
    }
    let sealed_pull_shutdown_completed = timeout(RESOURCE_PRODUCER_SHUTDOWN_TIMEOUT, async {
        while let Some(result) = sealed_pull_tasks.join_next().await {
            if let Err(error) = merge_sealed_pull_result(
                result,
                &mut pending,
                &mut sealed_pull_in_flight,
                &mut sealed_pull_waiting_bytes,
                &mut producer_shutdown,
            ) {
                drain_failure.get_or_insert(error);
            }
        }
    })
    .await
    .is_ok();
    if !sealed_pull_shutdown_completed {
        // A sealed pull task only owns an abortable producer handle; aborting
        // it requests producer cancellation through Drop. Drain the JoinSet so
        // no in-flight task is detached before the producer cleanup pass.
        sealed_pull_tasks.abort_all();
        while let Some(result) = sealed_pull_tasks.join_next().await {
            let _ = result;
        }
    }
    for completion in pending.values_mut() {
        if should_drain_sealed_on_disconnect(completion) {
            schedule_sealed_completion_drain(completion, &mut producer_shutdown);
        } else {
            schedule_sealed_completion_shutdown(completion, &mut producer_shutdown);
        }
    }
    let sealed_producer_shutdown_completed = timeout(RESOURCE_PRODUCER_SHUTDOWN_TIMEOUT, async {
        while let Some(joined) = producer_shutdown.join_next().await {
            let _ = joined;
        }
    })
    .await
    .is_ok();
    if !sealed_producer_shutdown_completed {
        // A started sealed producer owns the durable terminal commit. Keep the
        // cleanup JoinSet alive after the connection deadline rather than
        // aborting its drain task and dropping that producer.
        retain_shutdown_tasks_until_terminal(producer_shutdown).await;
    }
    match (result, drain_failure) {
        (Err(error), _) => Err(error),
        (Ok(()), Some(error)) => Err(error),
        (Ok(()), None) => Ok(()),
    }
}
fn drain_resource_completions<D: DispatchService>(
    dispatcher: &D,
    completion_receiver: &mut mpsc::Receiver<(u64, ResourceDispatchCompletion)>,
    pending: &mut BTreeMap<u64, ResourceDispatchCompletion>,
    cancelled: &mut BTreeMap<u64, (orna_protocol::ResourceCancel, RevisionPair)>,
    tasks: &mut BTreeMap<u64, ResourceTask>,
) -> BTreeSet<u64> {
    let mut completed = BTreeSet::new();
    while let Ok((stream_id, completion)) = completion_receiver.try_recv() {
        if let Some(request_id) = completion.actions.front().map(resource_action_request_id) {
            dispatcher.record_resource_terminal_provenance(
                stream_id,
                request_id,
                completion.terminal_provenance,
            );
        }
        if let Some(task) = tasks.get_mut(&stream_id) {
            task.active = false;
        }
        if store_resource_completion(stream_id, completion, pending, cancelled) {
            completed.insert(stream_id);
        }
    }
    completed
}

fn store_resource_completion(
    stream_id: u64,
    mut completion: ResourceDispatchCompletion,
    pending: &mut BTreeMap<u64, ResourceDispatchCompletion>,
    cancelled: &mut BTreeMap<u64, (orna_protocol::ResourceCancel, RevisionPair)>,
) -> bool {
    let mut producer = completion.producer.take();
    let completion_request_id = completion.actions.front().map(resource_action_request_id);
    if let Some((cancel, target_revision)) = cancelled.remove(&stream_id) {
        let completion_committed = completion_request_id.is_some_and(|request_id| {
            resource_completion_is_committed_for(&completion, request_id)
        });
        let pending_committed = pending.get(&stream_id).is_some_and(|existing| {
            resource_completion_is_committed_for(existing, cancel.request_id)
        });
        if completion_committed && !pending_committed {
            completion.producer = producer;
            pending.insert(stream_id, completion);
            return true;
        }
        if !pending_committed {
            pending.insert(
                stream_id,
                cancelled_resource_completion(cancel, target_revision, producer.take()),
            );
            return true;
        }
        return false;
    }
    completion.producer = producer;
    if let Some(existing) = pending.get(&stream_id) {
        let completion_committed = completion_request_id.is_some_and(|request_id| {
            resource_completion_is_committed_for(&completion, request_id)
        });
        if completion_committed
            && !resource_completion_is_committed_for(existing, completion_request_id.unwrap())
        {
            pending.insert(stream_id, completion);
            return true;
        }
        return false;
    }
    let is_terminal = completion_request_id
        .is_some_and(|request_id| resource_completion_is_committed_for(&completion, request_id))
        || completion.actions.iter().any(resource_action_is_terminal);
    pending.insert(stream_id, completion);
    is_terminal
}

fn cancelled_resource_completion(
    cancel: orna_protocol::ResourceCancel,
    target_revision: RevisionPair,
    producer: Option<AuthenticatedServerResourceProducer>,
) -> ResourceDispatchCompletion {
    ResourceDispatchCompletion {
        actions: VecDeque::from([ResourceServerFrame::Cancelled(
            orna_protocol::ResourceCancelled {
                stream_id: cancel.stream_id,
                request_id: cancel.request_id,
                target_revision,
                reason: cancel.reason,
            },
        )]),
        producer,
        producer_waiting_bytes: None,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
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

/// Applies the shared terminal-provenance rule to the direct resource socket.
///
/// Cancellation closes the local protocol state before a raced producer result
/// may be flushed. A syntactically valid late `Completed`/`Failed` frame still
/// wins only when the authenticated producer marked its terminal commit. Late
/// values and acceptance frames are drained, while an uncommitted terminal is
/// left to the cancellation path.
fn direct_resource_terminal_wins_after_cancel(
    disposition: ResourceFrameDisposition,
    action: &ResourceServerFrame,
    provenance: ResourceTerminalProvenance,
) -> bool {
    matches!(disposition, ResourceFrameDisposition::DroppedLate)
        && provenance.is_committed()
        && matches!(
            action,
            ResourceServerFrame::Completed(_) | ResourceServerFrame::Failed(_)
        )
}

fn resource_completion_is_committed_for(
    completion: &ResourceDispatchCompletion,
    request_id: orna_core::InvocationId,
) -> bool {
    completion.terminal_provenance.is_committed()
        && !completion.actions.is_empty()
        && completion
            .actions
            .iter()
            .all(|action| resource_action_request_id(action) == request_id)
        && completion.actions.iter().any(resource_action_is_terminal)
}

#[allow(clippy::too_many_arguments)]
async fn handle_resource_frame<D: DispatchService>(
    frame: ResourceClientFrame,
    reservation: PayloadReservation,
    dispatcher: &D,
    session: &AuthenticatedSession,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    connection: &mut ResourceProtocolConnection,
    pending: &mut BTreeMap<u64, ResourceDispatchCompletion>,
    cancelled: &mut BTreeMap<u64, (orna_protocol::ResourceCancel, RevisionPair)>,
    tasks: &mut BTreeMap<u64, ResourceTask>,
    producer_shutdown: &mut JoinSet<()>,
    requests: &mut BTreeMap<u64, ResourceRequest>,
    completion_sender: &mpsc::Sender<(u64, ResourceDispatchCompletion)>,
    completion_receiver: &mut mpsc::Receiver<(u64, ResourceDispatchCompletion)>,
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
        pending.get(&cancel.stream_id).is_some_and(|completion| {
            resource_completion_is_committed_for(completion, cancel.request_id)
        })
    });
    let mut cancellation_won = false;
    if let Some(cancel) = cancellation.filter(|_| !committed_completion) {
        drain_resource_completions(dispatcher, completion_receiver, pending, cancelled, tasks);
        committed_completion = pending.get(&cancel.stream_id).is_some_and(|completion| {
            resource_completion_is_committed_for(completion, cancel.request_id)
        });
        if !committed_completion && !cancelled.contains_key(&cancel.stream_id) {
            if let Some(request) = requests.get(&cancel.stream_id) {
                if request.request_id != cancel.request_id {
                    return Err(LocalRawSocketError::ResourceConnection {
                        source: ResourceConnectionError::MismatchedRequest {
                            stream_id: cancel.stream_id,
                        },
                    });
                }
                if !tasks.get(&cancel.stream_id).is_some_and(|task| task.active) {
                    let producer = pending
                        .get_mut(&cancel.stream_id)
                        .and_then(|completion| completion.producer.take());
                    if let Some(producer) = producer {
                        let guards = tasks
                            .get_mut(&cancel.stream_id)
                            .and_then(|task| task.guards.take());
                        schedule_resource_producer_shutdown(producer, producer_shutdown, guards);
                    }
                }
                if let Some(task) = tasks.get(&cancel.stream_id) {
                    if !task.active {
                        cancellation_won = true;
                    } else if !task.cancellation.request_cancel() {
                        if task.cancellation.is_acceptance_cancellation_requested() {
                            // Acceptance is still being committed. Preserve the
                            // cancellation marker so the pre-accept failure is
                            // surfaced as the protocol's terminal cancellation.
                            cancellation_won = true;
                        } else {
                            // A terminal commit that has started owns its result;
                            // keep the stream live so completion frames remain deliverable.
                            committed_completion = !task.cancellation.is_requested();
                            if !committed_completion {
                                return Ok(true);
                            }
                        }
                    } else {
                        cancellation_won = true;
                    }
                } else if pending.contains_key(&cancel.stream_id) {
                    // The dispatch task can be removed as soon as its completion is
                    // queued. If that completion is not currently deliverable (for
                    // example, a value is blocked on credit), cancellation still wins.
                    cancellation_won = true;
                } else {
                    // The local dispatch state can lag the protocol tombstone by one
                    // frame after a terminal completion is flushed. Let the protocol
                    // state classify that repeated control as late before rejecting
                    // it as an unknown local stream. A live protocol stream remains
                    // an error here because it has no corresponding dispatch state.
                    match connection.receive(ResourceClientFrame::Cancel(cancel)) {
                        Ok(ResourceFrameDisposition::DroppedLate) => return Ok(true),
                        Ok(ResourceFrameDisposition::Applied) => {
                            return Err(LocalRawSocketError::ResourceConnection {
                                source: ResourceConnectionError::UnknownStream {
                                    stream_id: cancel.stream_id,
                                },
                            });
                        }
                        Err(source) => {
                            return Err(LocalRawSocketError::ResourceConnection { source });
                        }
                    }
                }
            }
        }
    }
    let invalid_scalar_window_update = match &frame {
        ResourceClientFrame::WindowUpdate(update) => {
            requests.get(&update.stream_id).is_some_and(|request| {
                request.request_id == update.request_id
                    && request.resource_kind == ResourceKind::Single
            })
        }
        _ => false,
    };
    let disposition = if committed_completion {
        ResourceFrameDisposition::Applied
    } else {
        match connection.receive(frame) {
            Ok(disposition) => disposition,
            Err(ResourceConnectionError::WrongState { stream_id })
                if invalid_scalar_window_update =>
            {
                let request = requests
                    .get(&stream_id)
                    .expect("scalar resource request exists")
                    .clone();
                let pending_commit = pending.get(&stream_id).is_some_and(|completion| {
                    resource_completion_is_committed_for(completion, request.request_id)
                });
                if pending_commit || cancelled.contains_key(&stream_id) {
                    return Ok(true);
                }
                pending.insert(
                    stream_id,
                    ResourceDispatchCompletion {
                        actions: resource_internal_failure(&request),
                        producer: None,
                        producer_waiting_bytes: None,
                        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
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
        if tasks.get(&cancel.stream_id).is_some_and(|task| task.active) {
            // Let the late completion consume this marker and synthesize the
            // cancellation response exactly once.
            cancelled.insert(
                cancel.stream_id,
                (
                    cancel,
                    requests
                        .get(&cancel.stream_id)
                        .expect("cancelled resource request retained")
                        .target_revision,
                ),
            );
        } else {
            // The completion was already drained before cancellation won, so
            // there is no later producer event that needs a marker. Keep the
            // task guards until the terminal cancellation frame is flushed.
            pending.insert(
                cancel.stream_id,
                cancelled_resource_completion(
                    cancel,
                    requests
                        .get(&cancel.stream_id)
                        .expect("cancelled resource request retained")
                        .target_revision,
                    None,
                ),
            );
        }
    }
    if matches!(disposition, ResourceFrameDisposition::DroppedLate) {
        return Ok(true);
    }
    if let Some(request) = request {
        if !dispatcher.authorize_resource_request(&request) {
            requests.insert(request.stream_id, request.clone());
            pending.insert(
                request.stream_id,
                ResourceDispatchCompletion {
                    actions: resource_internal_failure(&request),
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                },
            );
            return Ok(true);
        }
        let operation = resources.reserve_kernel_operation()?;
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
                let mut completion = future.await;
                if task_cancellation.is_requested() {
                    if let Some(producer) = completion.producer.take() {
                        shutdown_resource_producer(producer).await;
                    }
                    completion = ResourceDispatchCompletion {
                        actions: VecDeque::new(),
                        producer: None,
                        producer_waiting_bytes: None,
                        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                    };
                }
                let _ = sender.send((stream_id, completion)).await;
            });
            requests.insert(stream_id, request.clone());
            tasks.insert(
                stream_id,
                ResourceTask {
                    handle,
                    cancellation,
                    active: true,
                    guards: Some(ResourceTaskGuards {
                        _operation: operation,
                        _payload: vec![reservation],
                    }),
                },
            );
        } else {
            requests.insert(request.stream_id, request.clone());
            pending.insert(
                request.stream_id,
                ResourceDispatchCompletion {
                    actions: resource_internal_failure(&request),
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
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
    tasks: &mut BTreeMap<u64, ResourceTask>,
    producer_shutdown: &mut JoinSet<()>,
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
                let keep_producer = pending
                    .get(&stream_id)
                    .is_some_and(|completion| completion.producer.is_some());
                let keep_task = tasks.get(&stream_id).is_some_and(|task| task.active);
                if !keep_producer && !keep_task {
                    pending.remove(&stream_id);
                    requests.remove(&stream_id);
                    tasks.remove(&stream_id);
                }
                break;
            };
            let mut candidate = connection.clone();
            let result = match &action {
                ResourceServerFrame::Cancelled(frame) => {
                    candidate.apply_cancelled_after_client_cancel(*frame)
                }
                _ => version.apply_resource(&mut candidate, action.clone()),
            };
            let disposition = match result {
                Ok(disposition) => {
                    *connection = candidate;
                    disposition
                }
                Err(ResourceConnectionError::InsufficientCredit { .. }) => break,
                Err(source) => {
                    let Some(request) = requests.get(&stream_id).cloned() else {
                        return Err(LocalRawSocketError::ResourceConnection { source });
                    };
                    if !pending_resource_error_is_local(&request, &action, &source) {
                        return Err(LocalRawSocketError::ResourceConnection { source });
                    }
                    let completion = pending
                        .get_mut(&stream_id)
                        .expect("pending resource completion exists");
                    completion.actions = resource_internal_failure(&request);
                    completion.producer_waiting_bytes = None;
                    continue;
                }
            };
            let publish_committed_late_terminal =
                pending.get(&stream_id).is_some_and(|completion| {
                    direct_resource_terminal_wins_after_cancel(
                        disposition,
                        &action,
                        completion.terminal_provenance,
                    )
                });
            match disposition {
                ResourceFrameDisposition::DroppedLate if !publish_committed_late_terminal => {
                    let terminal = resource_action_is_terminal(&action);
                    pending
                        .get_mut(&stream_id)
                        .expect("pending resource completion exists")
                        .actions
                        .pop_front();
                    if terminal {
                        requests.remove(&stream_id);
                        let (task_active, task_guards) = tasks
                            .get_mut(&stream_id)
                            .map(|task| {
                                if task.active {
                                    task.cancellation.request_cancel();
                                    (Some(true), None)
                                } else {
                                    (Some(false), task.guards.take())
                                }
                            })
                            .unwrap_or((None, None));
                        if task_active == Some(false) {
                            tasks.remove(&stream_id);
                        }
                        let producer = pending
                            .get_mut(&stream_id)
                            .and_then(|completion| completion.producer.take());
                        if let Some(producer) = producer {
                            schedule_resource_producer_shutdown(
                                producer,
                                producer_shutdown,
                                task_guards,
                            );
                        }
                    }
                    continue;
                }
                ResourceFrameDisposition::Applied | ResourceFrameDisposition::DroppedLate => {}
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
                let (task_active, task_guards) = tasks
                    .get_mut(&stream_id)
                    .map(|task| {
                        if task.active {
                            task.cancellation.request_cancel();
                            (Some(true), None)
                        } else {
                            (Some(false), task.guards.take())
                        }
                    })
                    .unwrap_or((None, None));
                if task_active == Some(false) {
                    tasks.remove(&stream_id);
                }
                let producer = pending
                    .get_mut(&stream_id)
                    .and_then(|completion| completion.producer.take());
                if let Some(producer) = producer {
                    schedule_resource_producer_shutdown(producer, producer_shutdown, task_guards);
                }
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
    cancelled_pending: &mut BTreeSet<u64>,
    preflight_pending: &mut BTreeSet<u64>,
    preflight_cancelled: &mut BTreeSet<u64>,
    preflight_tasks: &mut JoinSet<(u64, InvokePreflightCompletion)>,
    producer_shutdown: &mut JoinSet<()>,

    pending: &mut BTreeMap<u64, DispatchCompletion>,
    unstarted: &mut VecDeque<UnstartedDispatch>,
    socket: &mut OwnedWriteHalf,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let stream_id = client_stream(&incoming.frame);
    let retains_payload = matches!(
        incoming.frame,
        ClientFrame::CallRawStart { .. }
            | ClientFrame::CallArgument { .. }
            | ClientFrame::CallInvokeRequest { .. }
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
            let StartedDispatch {
                accepted, future, ..
            } = dispatcher.start(session.clone(), stream, call);
            let guards = DispatchGuards {
                _operation: dispatch_permit.expect("dispatch action requires reserved permit"),
                _payload: retained_payload.remove(&stream).unwrap_or_default(),
            };
            unstarted.push_back(UnstartedDispatch {
                stream,
                future,
                guards: Some(guards),
                defer_once: true,
            });
            let frame = version
                .apply(connection, accepted)
                .map_err(|source| LocalRawSocketError::Connection { source })?;
            if !write_server_frame(version, socket, &frame, shutdown).await? {
                return Ok(false);
            }
        }
        Some(ClientAction::InvokeDispatch { stream, request }) => {
            let preflight_session = session.clone();
            let preflight_request = request.clone();
            let preflight =
                dispatcher.preflight_invoke(preflight_session.clone(), request, version.clone());
            let guards = DispatchGuards {
                _operation: dispatch_permit.expect("dispatch action requires reserved permit"),
                _payload: retained_payload.remove(&stream).unwrap_or_default(),
            };
            preflight_pending.insert(stream);
            preflight_tasks.spawn(async move {
                let completion = InvokePreflightCompletion {
                    result: preflight.await,
                    session: preflight_session,
                    request: preflight_request,
                    guards,
                };
                (stream, completion)
            });
        }
        Some(ClientAction::Cancel { stream, .. }) => {
            // Accepted dispatches transfer their bytes into guards; any remaining
            // reservation is receiving-side state for this stream.
            retained_payload.remove(&stream);
            if let Some(completion) = pending.get_mut(&stream) {
                if !completion.terminal_delivered && !completion.terminal_claimed {
                    dispatcher.cancelled(stream);
                    schedule_sealed_completion_shutdown(completion, producer_shutdown);
                    queue_cancellation_actions(completion, stream);
                    if !completion.worker_completed {
                        cancelled_pending.insert(stream);
                    }
                }
            } else if preflight_pending.contains(&stream) {
                dispatcher.cancelled(stream);
                preflight_cancelled.insert(stream);
            } else {
                dispatcher.cancelled(stream);
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

#[allow(clippy::too_many_arguments)]
async fn finish_invoke_preflight<D: DispatchService>(
    stream: u64,
    completion: InvokePreflightCompletion,
    cancelled_before_accept: bool,
    dispatcher: &D,
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    unstarted: &mut VecDeque<UnstartedDispatch>,
    connection: &mut ProtocolConnection,
    version: &RawProtocolVersion,
    socket: &mut OwnedWriteHalf,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let InvokePreflightCompletion {
        result,
        session,
        request,
        guards,
    } = completion;

    let preflight_failure = match result {
        Ok(InvokePreflight::Rejected(failure)) => Some(failure),
        Err(source) => {
            report_private_dispatch_source(&source);
            Some(CallFailure::InternalFailure)
        }
        Ok(InvokePreflight::Accepted(continuation)) if cancelled_before_accept => {
            drop(continuation);
            None
        }
        Ok(InvokePreflight::Accepted(continuation)) => {
            let StartedDispatch {
                accepted,
                started,
                start_gate,
                future,
            } = dispatcher.start_invoke(session, stream, request, version, continuation);
            let started = started.expect("sealed invocation start event");
            let sealed_invocation = match &accepted {
                ServerAction::Accepted { invocation, .. } => *invocation,
                _ => unreachable!("sealed invocation dispatch must be accepted"),
            };
            pending.insert(
                stream,
                DispatchCompletion {
                    sealed_producer: None,
                    sealed_invocation: Some(sealed_invocation),
                    sealed_next_event_sequence: 1,
                    sealed_next_outer_sequence: 2,
                    actions: VecDeque::from([started]),
                    cancellation: ServerAction::InvokeCancelled { stream },
                    cancellation_token: None,
                    start_gate,
                    start_delivered: false,
                    terminal_delivered: false,
                    terminal_claimed: false,
                    worker_completed: false,
                    _guards: Some(guards),
                },
            );
            unstarted.push_back(UnstartedDispatch {
                stream,
                future,
                guards: None,
                defer_once: true,
            });
            let frame = version
                .apply(connection, accepted)
                .map_err(|source| LocalRawSocketError::Connection { source })?;
            return write_server_frame(version, socket, &frame, shutdown).await;
        }
    };

    let action = if cancelled_before_accept {
        ServerAction::Cancelled { stream }
    } else {
        ServerAction::Failed {
            stream,
            failure: preflight_failure.unwrap_or(CallFailure::InternalFailure),
        }
    };
    let terminal_claimed = matches!(
        &action,
        ServerAction::Failed {
            failure: CallFailure::InternalFailure,
            ..
        }
    );
    pending.insert(
        stream,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::from([action]),
            cancellation: ServerAction::Cancelled { stream },
            cancellation_token: None,
            start_gate: None,
            start_delivered: false,
            terminal_delivered: false,
            terminal_claimed,
            worker_completed: true,
            _guards: Some(guards),
        },
    );
    Ok(true)
}

fn start_one_dispatch(
    unstarted: &mut VecDeque<UnstartedDispatch>,
    tasks: &mut JoinSet<(u64, DispatchCompletion)>,
) {
    let dispatch = unstarted.pop_front().expect("unstarted dispatch exists");
    tasks.spawn(async move {
        let mut completion = dispatch.future.await;
        completion._guards = dispatch.guards;
        (dispatch.stream, completion)
    });
}

fn queue_sealed_terminal_failure(
    stream: u64,
    completion: &mut DispatchCompletion,
    invocation: InvocationId,
    phase: InvocationFailurePhase,
    code: &'static str,
    message: &'static str,
    retryability: InvocationRetryability,
) {
    let events = redacted_invoke_failure_at(
        invocation,
        completion.sealed_next_event_sequence,
        completion.sealed_next_outer_sequence,
        phase,
        code,
        message,
        retryability,
    );
    completion.sealed_next_event_sequence += 1;
    completion.sealed_next_outer_sequence += 1;
    completion.actions.extend([
        ServerAction::InvokeEvents { stream, events },
        ServerAction::Completed { stream },
    ]);
    completion.terminal_claimed = true;
}

// One value pull is wrapped in one ORV5 Event carrier. The producer reports
// the encoded value bytes, so reserve the fixed channel, record, carrier, and
// InvokeValue envelope bytes before granting value credit.
const SEALED_EVENT_FRAME_OVERHEAD: u64 = 1 + 2 // channel and record count
    + 8 + 1 + 4 // outer sequence, value kind, and content length
    + 25 // ORV5 Event carrier envelope
    + 1 + 1 + 16 + 8 // Event version, kind, invocation id, and sequence
    + 1 // ValueBatch body kind
    + 1 + 4 // absent schema marker and value count
    + 4 // embedded InvokeValue length
    + 25 // ORV5 InvokeValue envelope
    + 1 + 4; // InvokeValue version and inner value length
const SEALED_MAX_VALUE_BYTES: u64 =
    (MAX_FRAME_PAYLOAD_LENGTH as u64).saturating_sub(SEALED_EVENT_FRAME_OVERHEAD);

fn sealed_pull_credit(result_credit: u64, waiting_bytes: Option<u64>) -> Option<ResourceCredit> {
    let byte_credit = result_credit.min(SEALED_MAX_VALUE_BYTES);
    if waiting_bytes.is_some_and(|required| required > byte_credit) {
        return None;
    }
    ResourceCredit::new(1, byte_credit)
}

fn handle_sealed_producer_event(
    stream: u64,
    completion: &mut DispatchCompletion,
    waiting_bytes: &mut BTreeMap<u64, u64>,
    event: Result<AuthenticatedServerResourceEvent, PostgresKernelError>,
) {
    let invocation = completion
        .sealed_invocation
        .expect("sealed producer retains root invocation identity");
    let cancellation_requested = completion
        .cancellation_token
        .as_ref()
        .is_some_and(ResourceCancellation::is_requested)
        && !completion.terminal_claimed;
    match event {
        Ok(AuthenticatedServerResourceEvent::Values { values, .. }) => {
            waiting_bytes.remove(&stream);
            if cancellation_requested {
                return;
            }
            let [value] = match values.try_into() {
                Ok(values) => values,
                Err(_) => {
                    queue_sealed_terminal_failure(
                        stream,
                        completion,
                        invocation,
                        InvocationFailurePhase::Internal,
                        "INVOKE_INTERNAL_FAILURE",
                        "invocation could not complete",
                        InvocationRetryability::Unknown,
                    );
                    return;
                }
            };
            let value = match InvokeValue::new(value) {
                Ok(value) => value,
                Err(source) => {
                    report_private_dispatch_source(&PostgresKernelError::InvocationCarrier(source));
                    queue_sealed_terminal_failure(
                        stream,
                        completion,
                        invocation,
                        InvocationFailurePhase::Internal,
                        "INVOKE_INTERNAL_FAILURE",
                        "invocation could not complete",
                        InvocationRetryability::Unknown,
                    );
                    return;
                }
            };
            let event = match InvokeEvent::new(
                invocation,
                completion.sealed_next_event_sequence,
                InvocationEventBody::ValueBatch {
                    schema: None,
                    values: vec![value],
                },
            ) {
                Ok(event) => event,
                Err(source) => {
                    report_private_dispatch_source(&PostgresKernelError::InvocationCarrier(source));
                    queue_sealed_terminal_failure(
                        stream,
                        completion,
                        invocation,
                        InvocationFailurePhase::Internal,
                        "INVOKE_INTERNAL_FAILURE",
                        "invocation could not complete",
                        InvocationRetryability::Unknown,
                    );
                    return;
                }
            };
            completion.sealed_next_event_sequence += 1;
            let events = InvocationEventBatch::new(vec![InvocationEventRecord::new(
                completion.sealed_next_outer_sequence,
                event,
            )])
            .expect("bounded sealed ValueBatch event");
            completion.sealed_next_outer_sequence += 1;
            completion
                .actions
                .push_back(ServerAction::InvokeEvents { stream, events });
        }
        Ok(AuthenticatedServerResourceEvent::Completed { .. }) => {
            waiting_bytes.remove(&stream);
            completion.sealed_producer.take();
            if cancellation_requested {
                completion.actions.clear();
            }
            let event = InvokeEvent::new(
                invocation,
                completion.sealed_next_event_sequence,
                InvocationEventBody::Completed {
                    duration_nanoseconds: 0,
                },
            )
            .expect("bounded sealed completion event");
            let events = InvocationEventBatch::new(vec![InvocationEventRecord::new(
                completion.sealed_next_outer_sequence,
                event,
            )])
            .expect("bounded sealed completion batch");
            completion.sealed_next_event_sequence += 1;
            completion.sealed_next_outer_sequence += 1;
            completion.terminal_claimed = true;
            completion.actions.extend([
                ServerAction::InvokeEvents { stream, events },
                ServerAction::Completed { stream },
            ]);
        }
        Ok(AuthenticatedServerResourceEvent::Failed { failure }) => {
            waiting_bytes.remove(&stream);
            if cancellation_requested {
                completion.actions.clear();
            }
            let (phase, code, message, retryability) = match failure {
                CallFailure::TargetUnavailable => (
                    InvocationFailurePhase::Target,
                    "INVOKE_TARGET_FAILED",
                    "invocation target failed",
                    InvocationRetryability::Unknown,
                ),
                _ => (
                    InvocationFailurePhase::Internal,
                    "INVOKE_INTERNAL_FAILURE",
                    "invocation could not complete",
                    InvocationRetryability::Unknown,
                ),
            };
            queue_sealed_terminal_failure(
                stream,
                completion,
                invocation,
                phase,
                code,
                message,
                retryability,
            );
        }
        Ok(AuthenticatedServerResourceEvent::Cancelled) => {
            waiting_bytes.remove(&stream);
            completion.sealed_producer.take();
            completion.actions = cancellation_actions(stream, completion.cancellation.clone());
        }
        Ok(AuthenticatedServerResourceEvent::Waiting { required_bytes }) => {
            waiting_bytes.insert(stream, required_bytes);
        }
        Err(source) => {
            waiting_bytes.remove(&stream);
            report_private_dispatch_source(&source);
            if cancellation_requested {
                completion.actions.clear();
            }
            queue_sealed_terminal_failure(
                stream,
                completion,
                invocation,
                InvocationFailurePhase::Internal,
                "INVOKE_INTERNAL_FAILURE",
                "invocation could not complete",
                InvocationRetryability::Unknown,
            );
        }
    }
}

fn merge_sealed_pull_result(
    result: Result<SealedPullTaskResult, JoinError>,
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    sealed_pull_in_flight: &mut BTreeSet<u64>,
    sealed_pull_waiting_bytes: &mut BTreeMap<u64, u64>,
    producer_shutdown: &mut JoinSet<()>,
) -> Result<(), LocalRawSocketError> {
    let (stream, producer, event) =
        result.map_err(|source| LocalRawSocketError::DispatchTask { source })?;
    sealed_pull_in_flight.remove(&stream);
    if let Some(completion) = pending.get_mut(&stream) {
        completion.sealed_producer = Some(producer);
        handle_sealed_producer_event(stream, completion, sealed_pull_waiting_bytes, event);
    } else {
        // CALL_CANCEL or terminal delivery may have removed the completion while
        // this pull was in flight. The returned producer still owns a live
        // transaction; retain it in the bounded shutdown set instead of dropping
        // it at the merge boundary.
        schedule_shutdown_task(producer_shutdown, async move {
            shutdown_resource_producer(producer).await;
        });
    }
    Ok(())
}

#[cfg(test)]
async fn flush_pending(
    version: &RawProtocolVersion,
    connection: &mut ProtocolConnection,
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    sealed_pull_tasks: &mut JoinSet<SealedPullTaskResult>,
    sealed_pull_in_flight: &mut BTreeSet<u64>,
    sealed_pull_waiting_bytes: &mut BTreeMap<u64, u64>,
    producer_shutdown: &mut JoinSet<()>,
    stream: &mut OwnedWriteHalf,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let mut fairness_yielded = false;
    flush_pending_with_fairness_boundary(
        version,
        connection,
        pending,
        sealed_pull_tasks,
        sealed_pull_in_flight,
        sealed_pull_waiting_bytes,
        producer_shutdown,
        stream,
        shutdown,
        &mut fairness_yielded,
    )
    .await
}

async fn flush_pending_with_fairness_boundary(
    version: &RawProtocolVersion,
    connection: &mut ProtocolConnection,
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    sealed_pull_tasks: &mut JoinSet<SealedPullTaskResult>,
    sealed_pull_in_flight: &mut BTreeSet<u64>,
    sealed_pull_waiting_bytes: &mut BTreeMap<u64, u64>,
    producer_shutdown: &mut JoinSet<()>,
    stream: &mut OwnedWriteHalf,
    shutdown: &mut watch::Receiver<bool>,
    fairness_yielded: &mut bool,
) -> Result<bool, LocalRawSocketError> {
    *fairness_yielded = false;
    let mut flushed_actions = 0_usize;
    let stream_ids: Vec<_> = pending.keys().copied().collect();
    for stream_id in stream_ids {
        loop {
            if pending
                .get(&stream_id)
                .is_some_and(|completion| completion.worker_completed)
            {
                pending
                    .get_mut(&stream_id)
                    .expect("pending completion exists")
                    .release_guards_after_worker_completion();
            }
            let Some(action) = pending
                .get(&stream_id)
                .and_then(|completion| completion.actions.front())
                .cloned()
            else {
                if sealed_pull_waiting_bytes
                    .get(&stream_id)
                    .is_some_and(|required| *required > SEALED_MAX_VALUE_BYTES)
                {
                    let completion = pending
                        .get_mut(&stream_id)
                        .expect("pending completion exists");
                    let invocation = completion
                        .sealed_invocation
                        .expect("sealed producer retains root invocation identity");
                    sealed_pull_waiting_bytes.remove(&stream_id);
                    queue_sealed_terminal_failure(
                        stream_id,
                        completion,
                        invocation,
                        InvocationFailurePhase::Internal,
                        "INVOKE_INTERNAL_FAILURE",
                        "invocation could not complete",
                        InvocationRetryability::Unknown,
                    );
                    continue;
                }
                let should_pull = pending.get(&stream_id).is_some_and(|completion| {
                    completion.start_delivered
                        && completion.sealed_producer.is_some()
                        && !sealed_pull_in_flight.contains(&stream_id)
                });
                if should_pull {
                    let result_credit = connection
                        .result_credit(stream_id)
                        .map_err(|source| LocalRawSocketError::Connection { source })?;
                    let Some(credit) = sealed_pull_credit(
                        result_credit,
                        sealed_pull_waiting_bytes.get(&stream_id).copied(),
                    ) else {
                        break;
                    };
                    let producer = pending
                        .get_mut(&stream_id)
                        .expect("pending completion exists")
                        .sealed_producer
                        .take()
                        .expect("sealed producer exists");
                    // Admit one value per scheduler turn. The connection select
                    // runs before another pull, so a queued CALL_CANCEL or
                    // shutdown signal cannot be hidden behind a producer drain.
                    sealed_pull_in_flight.insert(stream_id);
                    sealed_pull_tasks.spawn(async move {
                        let event = producer.pull(credit).await;
                        (stream_id, producer, event)
                    });
                    break;
                }
                let keep_waiting = pending.get(&stream_id).is_some_and(|completion| {
                    completion.start_gate.is_some()
                        || (!completion.terminal_delivered && completion.start_delivered)
                        || sealed_pull_in_flight.contains(&stream_id)
                });
                if !keep_waiting {
                    pending.remove(&stream_id);
                }
                break;
            };
            let result_action = matches!(&action, ServerAction::Events { .. });
            let terminal_action = matches!(
                &action,
                ServerAction::Completed { .. }
                    | ServerAction::Failed { .. }
                    | ServerAction::Cancelled { .. }
                    | ServerAction::InvokeCancelled { .. }
            ) || matches!(
                &action,
                ServerAction::InvokeEvents { events, .. }
                    if events.records().iter().any(|record| {
                        matches!(
                            record.event().kind(),
                            InvocationEventKind::InvocationCompleted
                                | InvocationEventKind::InvocationFailed
                                | InvocationEventKind::InvocationCancelled
                        )
                    })
            );
            let worker_completed = pending
                .get(&stream_id)
                .is_some_and(|completion| completion.worker_completed);
            if terminal_action && (!worker_completed || sealed_pull_in_flight.contains(&stream_id))
            {
                break;
            }
            let frame = match version.apply(connection, action) {
                Ok(frame) => frame,
                Err(ConnectionError::InsufficientCredit { .. }) => break,
                Err(source) => return Err(LocalRawSocketError::Connection { source }),
            };
            if !write_server_frame(version, stream, &frame, shutdown).await? {
                return Ok(false);
            }
            let completion = pending
                .get_mut(&stream_id)
                .expect("pending completion exists");
            completion.actions.pop_front();
            if result_action {
                completion.terminal_claimed = false;
            }
            if let Some(gate) = completion.start_gate.take() {
                completion.start_delivered = true;
                let _ = gate.send(());
            }
            if terminal_action {
                completion.terminal_delivered = true;
                schedule_sealed_completion_shutdown(completion, producer_shutdown);
            }
            flushed_actions += 1;
            if flushed_actions >= PENDING_FLUSH_FAIRNESS_BUDGET {
                *fairness_yielded = true;
                return Ok(true);
            }
        }
    }
    Ok(true)
}

fn spawn_frame_reader(
    mut reader: OwnedReadHalf,
    version: RawProtocolVersion,
    resources: LocalRawSocketResources,
    resource_read_state: ResourceReadState,
    sender: mpsc::Sender<Result<Option<IncomingFrame>, LocalRawSocketError>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let frame = read_versioned_client_frame_with_resource_state(
                &mut reader,
                &version,
                &resources,
                Instant::now() + FRAME_IDLE_TIMEOUT,
                &resource_read_state,
            )
            .await;
            let terminal = !matches!(frame, Ok(Some(_)));
            if sender.send(frame).await.is_err() || terminal {
                return;
            }
        }
    })
}

#[derive(Clone, Copy)]
enum FrameReadMode<'a> {
    #[cfg(test)]
    Fixed(Instant),
    ResourceAware {
        deadline: Instant,
        state: &'a ResourceReadState,
    },
}

impl FrameReadMode<'_> {
    fn deadline(self) -> Instant {
        match self {
            #[cfg(test)]
            Self::Fixed(deadline) => deadline,
            Self::ResourceAware { deadline, .. } => deadline,
        }
    }

    fn resource_active(self) -> bool {
        match self {
            #[cfg(test)]
            Self::Fixed(_) => false,
            Self::ResourceAware { state, .. } => state.is_active(),
        }
    }
}

fn resource_idle_timeout_is_retryable(
    resource_active: bool,
    header_bytes: usize,
    deadline: Instant,
    now: Instant,
) -> bool {
    resource_active && header_bytes == 0 && now >= deadline
}

#[cfg(test)]
async fn read_client_frame<R: AsyncRead + Unpin>(
    stream: &mut R,
    resources: &LocalRawSocketResources,
    deadline: Instant,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    read_versioned_client_frame(stream, &RawProtocolVersion::One, resources, deadline).await
}

#[cfg(test)]
async fn read_versioned_client_frame<R: AsyncRead + Unpin>(
    stream: &mut R,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    deadline: Instant,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    read_versioned_client_frame_with_mode(
        stream,
        version,
        resources,
        FrameReadMode::Fixed(deadline),
    )
    .await
}

async fn read_versioned_client_frame_with_resource_state<R: AsyncRead + Unpin>(
    stream: &mut R,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    deadline: Instant,
    state: &ResourceReadState,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    read_versioned_client_frame_with_mode(
        stream,
        version,
        resources,
        FrameReadMode::ResourceAware { deadline, state },
    )
    .await
}

async fn read_versioned_client_frame_with_mode<R: AsyncRead + Unpin>(
    stream: &mut R,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    mode: FrameReadMode<'_>,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    let mut header = [0_u8; SESSION_HEADER_LENGTH];
    let Some(frame_deadline) =
        read_header_before(stream, &mut header[..SESSION_MARKER.len()], mode).await?
    else {
        return Ok(None);
    };
    let session = &header[..SESSION_MARKER.len()] == SESSION_MARKER;
    if !session {
        read_exact_before(
            stream,
            &mut header[SESSION_MARKER.len()..RESOURCE_MARKER.len()],
            frame_deadline,
            LocalRawSocketError::FrameTimeout,
        )
        .await?;
    }
    let resource = !session && &header[..RESOURCE_MARKER.len()] == RESOURCE_MARKER;
    let header_length = if session {
        SESSION_HEADER_LENGTH
    } else if resource {
        RESOURCE_HEADER_LENGTH
    } else {
        FRAME_HEADER_LENGTH
    };
    let consumed = if session {
        SESSION_MARKER.len()
    } else {
        RESOURCE_MARKER.len()
    };
    read_exact_before(
        stream,
        &mut header[consumed..header_length],
        frame_deadline,
        LocalRawSocketError::FrameTimeout,
    )
    .await?;
    let declared_offset = if session {
        SESSION_HEADER_LENGTH - std::mem::size_of::<u32>()..SESSION_HEADER_LENGTH
    } else if resource {
        17..21
    } else {
        14..18
    };
    let declared = u32::from_be_bytes(
        header[declared_offset]
            .try_into()
            .expect("fixed frame header"),
    ) as usize;
    if session && declared > orna_protocol::MAX_SESSION_FRAME_LENGTH - SESSION_HEADER_LENGTH {
        return Err(LocalRawSocketError::Session {
            source: SessionCodecError::Oversize,
        });
    }
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
        frame_deadline,
        LocalRawSocketError::FrameTimeout,
    )
    .await?;
    if session {
        let frame = decode_session_client_frame(&encoded)
            .map_err(|source| LocalRawSocketError::Session { source })?;
        Ok(Some(IncomingFrame::Session { frame, reservation }))
    } else if resource {
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
    mode: FrameReadMode<'_>,
) -> Result<Option<Instant>, LocalRawSocketError> {
    let mut filled = 0;
    let mut deadline = mode.deadline();
    while filled < header.len() {
        let read = match timeout_at(deadline, stream.read(&mut header[filled..])).await {
            Ok(read) => read.map_err(|source| LocalRawSocketError::Io { source })?,
            Err(_)
                if resource_idle_timeout_is_retryable(
                    mode.resource_active(),
                    filled,
                    deadline,
                    Instant::now(),
                ) =>
            {
                // A live resource may be waiting indefinitely for client credit.
                // Once a frame has started, the fresh deadline below bounds the
                // remainder and prevents an incomplete frame from pinning a task.
                deadline = Instant::now() + FRAME_IDLE_TIMEOUT;
                continue;
            }
            Err(_) => return Err(LocalRawSocketError::FrameTimeout),
        };
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
        if filled == 0 && mode.resource_active() {
            deadline = Instant::now() + FRAME_IDLE_TIMEOUT;
        }
        filled += read;
    }
    Ok(Some(deadline))
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

async fn flush_session_pending<D: DispatchService>(
    dispatcher: &D,
    stream: &mut OwnedWriteHalf,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let Some(bridge) = dispatcher.session_bridge() else {
        return Ok(true);
    };
    while let Some(frame) = bridge.try_take_outbound() {
        let encoded = encode_session_server_frame(&frame)
            .map_err(|source| LocalRawSocketError::Session { source })?;
        if !write_all_until_shutdown(stream, &encoded, shutdown).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn wait_for_session_outbound(bridge: Option<Arc<crate::invoke::SessionBridge>>) {
    match bridge {
        Some(bridge) => bridge.wait_for_outbound().await,
        None => std::future::pending::<()>().await,
    }
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
        | ClientFrame::CallInvokeRequest { stream, .. }
        | ClientFrame::CallArgumentsComplete { stream }
        | ClientFrame::WindowUpdate { stream, .. }
        | ClientFrame::CallCancel { stream } => *stream,
        ClientFrame::Ping { .. } => 0,
    }
}

#[cfg(test)]
mod tests;
