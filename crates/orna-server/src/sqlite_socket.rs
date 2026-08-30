//! Local SQLite raw-call socket service.
//!
//! The SQLite service deliberately shares the public raw-call wire protocol with
//! the PostgreSQL service, but keeps its execution surface fail-closed: only
//! calls accepted by [`SqliteRevisionStore::execute_server_function`] can
//! produce a successful result.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs, io,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::UnixStream as StandardUnixStream,
    },
    path::{Path, PathBuf},
    sync::Arc,
};

use orna_core::{
    InvocationId, ParameterId, catalogue::CatalogueSnapshot, revision::ActiveDatabaseRevision,
    value::OpaqueCodecRegistry,
};
use orna_protocol::{
    CallFailure, ClientAction, ClientFrame, ConnectionError, Event, FrameCodecError,
    MAX_CHANNEL_WINDOW, MAX_FRAME_PAYLOAD_LENGTH, ProtocolConnection, RawCall, ServerAction,
    ServerFrame, decode_active_client_frame, decode_catalogue_client_frame, decode_client_frame,
    decode_constructed_client_frame, decode_registered_client_frame, encode_active_server_frame,
    encode_catalogue_server_frame, encode_constructed_server_frame, encode_registered_server_frame,
    encode_server_frame,
};
use orna_sqlite::{SqliteConfig, SqliteError, SqliteRevisionStore};
use orna_standard::registered_opaque_codecs;
use orna_storage::{ApplicationRevisionStore, StorageError};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    signal::unix::{SignalKind, signal},
    sync::Semaphore,
    task::JoinSet,
    time::{Duration, timeout},
};

const CLIENT_HELLO_V1: [u8; 12] = *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00";
const CLIENT_HELLO_V2: [u8; 12] = *b"ORNA\x01\x00\x00\x02\x00\x00\x00\x00";
const CLIENT_HELLO_V3: [u8; 12] = *b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00";
const CLIENT_HELLO_V4: [u8; 12] = *b"ORNA\x01\x00\x00\x04\x00\x00\x00\x00";
const CLIENT_HELLO_V5: [u8; 12] = *b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00";
const SERVER_ACK_V1: [u8; 12] = *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00";
const SERVER_ACK_V2: [u8; 12] = *b"ORNA\x81\x00\x00\x02\x00\x00\x00\x00";
const SERVER_ACK_V3: [u8; 12] = *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00";
const SERVER_ACK_V4: [u8; 12] = *b"ORNA\x81\x00\x00\x04\x00\x00\x00\x00";
const SERVER_ACK_V5: [u8; 12] = *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00";

const FRAME_HEADER_LENGTH: usize = 18;
const CONNECTION_LIMIT: usize = 64;
const MAX_FALLBACK_LIVE_STREAMS: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const SOCKET_MODE: u32 = 0o600;

const CALL_RAW_START_TAG: u8 = 0x01;
const CALL_ARGUMENT_TAG: u8 = 0x02;
const CALL_ARGUMENTS_COMPLETE_TAG: u8 = 0x03;
const WINDOW_UPDATE_TAG: u8 = 0x04;
const CALL_CANCEL_TAG: u8 = 0x05;
const PING_TAG: u8 = 0x06;
const CALL_FAILED_TAG: u8 = 0x84;
const CALL_CANCELLED_TAG: u8 = 0x85;
const PONG_TAG: u8 = 0x86;
const INTERNAL_FAILURE_WIRE: [u8; 4] = [0xff, 0x00, 0x01, 0x00];

#[derive(Debug)]
pub struct SqliteSocketError {
    message: String,
}

impl SqliteSocketError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(context: &'static str, source: io::Error) -> Self {
        Self::new(format!("orna: SQLite socket {context}: {source}"))
    }

    fn protocol(context: &'static str) -> Self {
        Self::new(format!("orna: SQLite socket {context}"))
    }
}

impl fmt::Display for SqliteSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SqliteSocketError {}

/// Runs the local SQLite raw-call server for one database path.
///
/// The database is opened and bootstrapped before the socket is made visible.
/// The public socket is the database path with `.orna.sock` appended. The
/// listener remains in the foreground until its accept loop is interrupted or
/// closed by the operating system.
///
/// # Errors
///
/// Returns [`SqliteSocketError`] when SQLite cannot be opened or bootstrapped,
/// the socket path is unsafe or cannot be bound, the runtime cannot start, or
/// the listener itself fails.
pub fn run_sqlite_server(database_path: impl Into<PathBuf>) -> Result<(), SqliteSocketError> {
    let database_path = database_path.into();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| {
            SqliteSocketError::new(format!("orna: SQLite socket runtime failed: {source}"))
        })?;
    runtime.block_on(run_sqlite_server_async(database_path))
}

async fn run_sqlite_server_async(database_path: PathBuf) -> Result<(), SqliteSocketError> {
    let socket_path = socket_path(&database_path);
    // Probe the derived socket before opening SQLite so a live server is
    // reported as a socket conflict rather than as a database lock failure.
    remove_stale_socket(&socket_path)?;
    let store = SqliteRevisionStore::open(&SqliteConfig::new(&database_path))
        .await
        .map_err(|source| sqlite_error("could not open SQLite database", source))?;
    ApplicationRevisionStore::bootstrap(&store)
        .await
        .map_err(|source| storage_error("could not bootstrap SQLite database", source))?;
    let active = ApplicationRevisionStore::recover(&store)
        .await
        .map_err(|source| storage_error("could not recover SQLite database", source))?;
    let versions = ProtocolVersions::new(active);
    let (listener, _cleanup) = bind_socket(&socket_path)?;
    run_listener(listener, Arc::new(store), versions).await
}

fn sqlite_error(context: &'static str, source: SqliteError) -> SqliteSocketError {
    SqliteSocketError::new(format!("orna: SQLite socket {context}: {source}"))
}

fn sqlite_call_failure(error: &SqliteError) -> CallFailure {
    match error {
        SqliteError::Domain(_) => CallFailure::TargetUnavailable,
        SqliteError::Backend(_)
        | SqliteError::InvalidPersistedData(_)
        | SqliteError::UnsupportedCapability(_) => CallFailure::InternalFailure,
    }
}

fn storage_error(context: &'static str, source: StorageError<SqliteError>) -> SqliteSocketError {
    SqliteSocketError::new(format!("orna: SQLite socket {context}: {source}"))
}

fn socket_path(database_path: &Path) -> PathBuf {
    let mut path = database_path.as_os_str().to_os_string();
    path.push(".orna.sock");
    PathBuf::from(path)
}

struct SocketCleanup {
    path: PathBuf,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = sync_socket_parent(&self.path);
    }
}

fn sync_socket_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    fs::File::open(parent).and_then(|directory| directory.sync_all())
}

fn bind_socket(path: &Path) -> Result<(UnixListener, SocketCleanup), SqliteSocketError> {
    remove_stale_socket(path)?;
    let listener = UnixListener::bind(path)
        .map_err(|source| SqliteSocketError::io("could not bind", source))?;
    let cleanup = SocketCleanup {
        path: path.to_path_buf(),
    };
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE)) {
        drop(cleanup);
        return Err(SqliteSocketError::io(
            "could not set socket permissions",
            error,
        ));
    }
    if let Err(error) = verify_socket_mode(path) {
        drop(cleanup);
        return Err(error);
    }
    sync_socket_parent(path)
        .map_err(|source| SqliteSocketError::io("could not sync socket directory", source))?;
    Ok((listener, cleanup))
}

fn verify_socket_mode(path: &Path) -> Result<(), SqliteSocketError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| SqliteSocketError::io("could not inspect socket", source))?;
    if !metadata.file_type().is_socket() || metadata.permissions().mode() & 0o7777 != SOCKET_MODE {
        return Err(SqliteSocketError::protocol(
            "socket path has invalid metadata",
        ));
    }
    Ok(())
}

fn remove_stale_socket(path: &Path) -> Result<(), SqliteSocketError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(SqliteSocketError::io(
                "could not inspect existing socket",
                source,
            ));
        }
    };
    if !metadata.file_type().is_socket() {
        return Err(SqliteSocketError::protocol(
            "socket path is occupied by a non-socket",
        ));
    }

    match StandardUnixStream::connect(path) {
        Ok(_live_peer) => Err(SqliteSocketError::protocol(
            "socket already has a live server",
        )),
        Err(source) if source.kind() == io::ErrorKind::ConnectionRefused => {
            fs::remove_file(path)
                .map_err(|error| SqliteSocketError::io("could not remove stale socket", error))?;
            sync_socket_parent(path).map_err(|error| {
                SqliteSocketError::io("could not sync stale socket removal", error)
            })?;
            Ok(())
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SqliteSocketError::io(
            "could not probe existing socket",
            source,
        )),
    }
}

#[derive(Clone)]
struct ProtocolVersions {
    catalogue: Arc<CatalogueSnapshot>,
    active: Arc<ActiveDatabaseRevision>,
    registry: Option<Arc<OpaqueCodecRegistry>>,
}

impl ProtocolVersions {
    fn new(active: ActiveDatabaseRevision) -> Self {
        let active = Arc::new(active);
        let catalogue = Arc::new(active.catalogue().clone());
        let registry = active
            .catalogue_hash_context()
            .standard()
            .and_then(|standard| registered_opaque_codecs(standard).ok())
            .map(Arc::new);
        Self {
            catalogue,
            active,
            registry,
        }
    }

    fn select(&self, requested: RequestedVersion) -> ProtocolVersion {
        match requested {
            RequestedVersion::V1 => ProtocolVersion::One,
            RequestedVersion::V2 => ProtocolVersion::Catalogue(Arc::clone(&self.catalogue)),
            RequestedVersion::V3 => ProtocolVersion::Active(Arc::clone(&self.active)),
            RequestedVersion::V4 => self
                .registry
                .as_ref()
                .map(|registry| ProtocolVersion::Registered {
                    active: Arc::clone(&self.active),
                    registry: Arc::clone(registry),
                })
                .unwrap_or(ProtocolVersion::Fallback(*b"ORF4")),
            RequestedVersion::V5 => self
                .registry
                .as_ref()
                .map(|registry| ProtocolVersion::Constructed {
                    active: Arc::clone(&self.active),
                    registry: Arc::clone(registry),
                })
                .unwrap_or(ProtocolVersion::Fallback(*b"ORF5")),
        }
    }
}

#[derive(Clone)]
enum ProtocolVersion {
    One,
    Catalogue(Arc<CatalogueSnapshot>),
    Active(Arc<ActiveDatabaseRevision>),
    Registered {
        active: Arc<ActiveDatabaseRevision>,
        registry: Arc<OpaqueCodecRegistry>,
    },
    Constructed {
        active: Arc<ActiveDatabaseRevision>,
        registry: Arc<OpaqueCodecRegistry>,
    },
    Fallback([u8; 4]),
}

impl ProtocolVersion {
    fn decode_client_frame(&self, encoded: &[u8]) -> Result<ClientFrame, FrameCodecError> {
        match self {
            Self::One => decode_client_frame(encoded),
            Self::Catalogue(catalogue) => decode_catalogue_client_frame(catalogue, encoded),
            Self::Active(active) => decode_active_client_frame(active, encoded),
            Self::Registered { active, registry } => {
                decode_registered_client_frame(active, registry, encoded)
            }
            Self::Constructed { active, registry } => {
                decode_constructed_client_frame(active, registry, encoded)
            }
            Self::Fallback(_) => Err(FrameCodecError::InvalidMarker),
        }
    }

    fn encode_server_frame(&self, frame: &ServerFrame) -> Result<Vec<u8>, FrameCodecError> {
        match self {
            Self::One => encode_server_frame(frame),
            Self::Catalogue(catalogue) => encode_catalogue_server_frame(catalogue, frame),
            Self::Active(active) => encode_active_server_frame(active, frame),
            Self::Registered { active, registry } => {
                encode_registered_server_frame(active, registry, frame)
            }
            Self::Constructed { active, registry } => {
                encode_constructed_server_frame(active, registry, frame)
            }
            Self::Fallback(_) => Err(FrameCodecError::InvalidMarker),
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
            Self::Registered { active, registry } => {
                connection.receive_registered(active, registry, frame)
            }
            Self::Constructed { active, registry } => {
                connection.receive_constructed(active, registry, frame)
            }
            Self::Fallback(_) => Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::InvalidMarker,
            }),
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
            Self::Registered { active, registry } => {
                connection.apply_registered(active, registry, action)
            }
            Self::Constructed { active, registry } => {
                connection.apply_constructed(active, registry, action)
            }
            Self::Fallback(_) => Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::InvalidMarker,
            }),
        }
    }

    fn rejects_constructed_calls(&self) -> bool {
        matches!(self, Self::Constructed { .. } | Self::Fallback(_))
    }
}

#[derive(Clone, Copy)]
enum RequestedVersion {
    V1,
    V2,
    V3,
    V4,
    V5,
}

impl RequestedVersion {
    fn from_hello(hello: &[u8; 12]) -> Option<Self> {
        match *hello {
            CLIENT_HELLO_V1 => Some(Self::V1),
            CLIENT_HELLO_V2 => Some(Self::V2),
            CLIENT_HELLO_V3 => Some(Self::V3),
            CLIENT_HELLO_V4 => Some(Self::V4),
            CLIENT_HELLO_V5 => Some(Self::V5),
            _ => None,
        }
    }

    const fn acknowledgement(self) -> &'static [u8; 12] {
        match self {
            Self::V1 => &SERVER_ACK_V1,
            Self::V2 => &SERVER_ACK_V2,
            Self::V3 => &SERVER_ACK_V3,
            Self::V4 => &SERVER_ACK_V4,
            Self::V5 => &SERVER_ACK_V5,
        }
    }
}

async fn run_listener(
    listener: UnixListener,
    store: Arc<SqliteRevisionStore>,
    versions: ProtocolVersions,
) -> Result<(), SqliteSocketError> {
    let mut interrupt = signal(SignalKind::interrupt())
        .map_err(|source| SqliteSocketError::io("could not install SIGINT handler", source))?;
    let mut terminate = signal(SignalKind::terminate())
        .map_err(|source| SqliteSocketError::io("could not install SIGTERM handler", source))?;
    let connections = Arc::new(Semaphore::new(CONNECTION_LIMIT));
    let mut workers = JoinSet::new();
    let result = loop {
        tokio::select! {
            _ = interrupt.recv() => break Ok(()),
            _ = terminate.recv() => break Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(connection) => connection,
                    Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
                    Err(source) => break Err(SqliteSocketError::io("accept failed", source)),
                };
                let Ok(connection_permit) = Arc::clone(&connections).try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let store = Arc::clone(&store);
                let versions = versions.clone();
                workers.spawn(async move {
                    let _connection_permit = connection_permit;
                    let _ = serve_connection(store, versions, stream).await;
                });
            }
            finished = workers.join_next(), if !workers.is_empty() => {
                if let Some(Err(source)) = finished {
                    break Err(SqliteSocketError::new(format!("orna: SQLite socket connection task failed: {source}")));
                }
            }
        }
    };
    workers.abort_all();
    while workers.join_next().await.is_some() {}
    result
}

async fn serve_connection(
    store: Arc<SqliteRevisionStore>,
    versions: ProtocolVersions,
    mut stream: UnixStream,
) -> Result<(), SqliteSocketError> {
    let mut hello = [0_u8; 12];
    let hello_eof = match timeout(
        HANDSHAKE_TIMEOUT,
        read_exact_or_eof(&mut stream, &mut hello),
    )
    .await
    {
        Ok(Ok(eof)) => eof,
        Ok(Err(_)) | Err(_) => return Ok(()),
    };
    if hello_eof {
        return Ok(());
    }
    let Some(requested) = RequestedVersion::from_hello(&hello) else {
        return Ok(());
    };
    stream
        .write_all(requested.acknowledgement())
        .await
        .map_err(|source| {
            SqliteSocketError::io("could not send handshake acknowledgement", source)
        })?;

    match versions.select(requested) {
        ProtocolVersion::Fallback(marker) => serve_fallback(stream, marker).await,
        version => serve_typed(store, version, stream).await,
    }
}

async fn serve_typed(
    store: Arc<SqliteRevisionStore>,
    version: ProtocolVersion,
    mut stream: UnixStream,
) -> Result<(), SqliteSocketError> {
    let mut connection = ProtocolConnection::new();
    loop {
        let Some(encoded) = next_frame(&mut stream).await? else {
            return Ok(());
        };
        let frame = match version.decode_client_frame(&encoded) {
            Ok(frame) => frame,
            Err(_) => return Ok(()),
        };
        let action = match version.receive(&mut connection, frame) {
            Ok(action) => action,
            Err(_) => return Ok(()),
        };
        let Some(action) = action else {
            continue;
        };
        match action {
            ClientAction::Send(frame) => write_typed_frame(&version, &mut stream, &frame).await?,
            ClientAction::Dispatch {
                stream: call_stream,
                call,
            } => {
                if version.rejects_constructed_calls() {
                    send_typed_failure(
                        &version,
                        &mut connection,
                        &mut stream,
                        call_stream,
                        CallFailure::InternalFailure,
                    )
                    .await?;
                } else {
                    dispatch_call(
                        &store,
                        &version,
                        &mut connection,
                        &mut stream,
                        call_stream,
                        call,
                    )
                    .await?;
                }
            }
            ClientAction::InvokeDispatch {
                stream: call_stream,
                ..
            } => {
                send_typed_failure(
                    &version,
                    &mut connection,
                    &mut stream,
                    call_stream,
                    CallFailure::InternalFailure,
                )
                .await?;
            }
            ClientAction::Cancel {
                stream: call_stream,
                invocation,
            } => {
                if invocation.is_some() {
                    return Ok(());
                }
                let frame = version
                    .apply(
                        &mut connection,
                        ServerAction::Cancelled {
                            stream: call_stream,
                        },
                    )
                    .map_err(|_| SqliteSocketError::protocol("could not cancel call"))?;
                write_typed_frame(&version, &mut stream, &frame).await?;
            }
        }
    }
}

async fn dispatch_call(
    store: &SqliteRevisionStore,
    version: &ProtocolVersion,
    connection: &mut ProtocolConnection,
    stream: &mut UnixStream,
    call_stream: u64,
    call: RawCall,
) -> Result<(), SqliteSocketError> {
    let arguments: Vec<(ParameterId, orna_core::value::RuntimeValue)> = call
        .arguments
        .into_iter()
        .map(|argument| (argument.parameter, argument.value))
        .collect();
    let values = match store
        .execute_server_function(call.function, &arguments)
        .await
    {
        Ok(values) => values,
        Err(error) => {
            send_typed_failure(
                version,
                connection,
                stream,
                call_stream,
                sqlite_call_failure(&error),
            )
            .await?;
            return Ok(());
        }
    };

    let accepted = version
        .apply(
            connection,
            ServerAction::Accepted {
                stream: call_stream,
                invocation: InvocationId::new(),
            },
        )
        .map_err(|_| SqliteSocketError::protocol("could not accept call"))?;
    write_typed_frame(version, stream, &accepted).await?;

    if !values.is_empty() {
        let events = values.into_iter().map(Event::Value).collect();
        let event_frame = match version.apply(
            connection,
            ServerAction::Events {
                stream: call_stream,
                events,
            },
        ) {
            Ok(frame) => frame,
            Err(_) => {
                send_typed_failure(
                    version,
                    connection,
                    stream,
                    call_stream,
                    CallFailure::InternalFailure,
                )
                .await?;
                return Ok(());
            }
        };
        write_typed_frame(version, stream, &event_frame).await?;
    }

    let completed = version
        .apply(
            connection,
            ServerAction::Completed {
                stream: call_stream,
            },
        )
        .map_err(|_| SqliteSocketError::protocol("could not complete call"))?;
    write_typed_frame(version, stream, &completed).await
}

async fn send_typed_failure(
    version: &ProtocolVersion,
    connection: &mut ProtocolConnection,
    output: &mut UnixStream,
    call_stream: u64,
    failure: CallFailure,
) -> Result<(), SqliteSocketError> {
    let frame = version
        .apply(
            connection,
            ServerAction::Failed {
                stream: call_stream,
                failure,
            },
        )
        .map_err(|_| SqliteSocketError::protocol("could not report call failure"))?;
    write_typed_frame(version, output, &frame).await
}

async fn write_typed_frame(
    version: &ProtocolVersion,
    stream: &mut UnixStream,
    frame: &ServerFrame,
) -> Result<(), SqliteSocketError> {
    let encoded = version
        .encode_server_frame(frame)
        .map_err(|_| SqliteSocketError::protocol("could not encode server frame"))?;
    stream
        .write_all(&encoded)
        .await
        .map_err(|source| SqliteSocketError::io("could not write server frame", source))
}

async fn next_frame(stream: &mut UnixStream) -> Result<Option<Vec<u8>>, SqliteSocketError> {
    match timeout(FRAME_IDLE_TIMEOUT, read_frame(stream)).await {
        Ok(Ok(frame)) => Ok(frame),
        Ok(Err(source)) => Err(SqliteSocketError::io("could not read frame", source)),
        Err(_) => Err(SqliteSocketError::protocol("frame read timed out")),
    }
}

async fn read_frame(stream: &mut UnixStream) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0_u8; FRAME_HEADER_LENGTH];
    if read_exact_or_eof(stream, &mut header).await? {
        return Ok(None);
    }
    let payload_length = u32::from_be_bytes(
        header[14..18]
            .try_into()
            .expect("frame header length is fixed"),
    ) as usize;
    if payload_length > MAX_FRAME_PAYLOAD_LENGTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame payload exceeds protocol limit",
        ));
    }
    let mut encoded = Vec::with_capacity(FRAME_HEADER_LENGTH + payload_length);
    encoded.extend_from_slice(&header);
    encoded.resize(FRAME_HEADER_LENGTH + payload_length, 0);
    if payload_length != 0 && read_exact_or_eof(stream, &mut encoded[FRAME_HEADER_LENGTH..]).await?
    {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "stream closed in a protocol envelope",
        ));
    }
    Ok(Some(encoded))
}

async fn read_exact_or_eof<R: AsyncRead + Unpin>(
    stream: &mut R,
    buffer: &mut [u8],
) -> io::Result<bool> {
    let mut offset = 0;
    while offset < buffer.len() {
        let read = stream.read(&mut buffer[offset..]).await?;
        if read == 0 {
            if offset == 0 {
                return Ok(true);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream closed in a protocol envelope",
            ));
        }
        offset += read;
    }
    Ok(false)
}

#[derive(Clone, Copy)]
struct RawFrame<'a> {
    tag: u8,
    stream: u64,
    payload: &'a [u8],
}

fn decode_raw_frame<'a>(marker: &[u8; 4], encoded: &'a [u8]) -> Result<RawFrame<'a>, ()> {
    if encoded.len() < FRAME_HEADER_LENGTH || &encoded[..4] != marker || encoded[5] != 0 {
        return Err(());
    }
    let stream = u64::from_be_bytes(encoded[6..14].try_into().map_err(|_| ())?);
    let declared = u32::from_be_bytes(encoded[14..18].try_into().map_err(|_| ())?) as usize;
    if declared > MAX_FRAME_PAYLOAD_LENGTH
        || encoded.len() != FRAME_HEADER_LENGTH.saturating_add(declared)
    {
        return Err(());
    }
    Ok(RawFrame {
        tag: encoded[4],
        stream,
        payload: &encoded[FRAME_HEADER_LENGTH..],
    })
}

struct FallbackConnection {
    marker: [u8; 4],
    high_water_mark: Option<u64>,
    streams: BTreeMap<u64, [u64; 6]>,
}

impl FallbackConnection {
    fn new(marker: [u8; 4]) -> Self {
        Self {
            marker,
            high_water_mark: None,
            streams: BTreeMap::new(),
        }
    }

    fn handle(&mut self, frame: RawFrame<'_>) -> Result<Option<Vec<u8>>, ()> {
        match frame.tag {
            PING_TAG => {
                if frame.stream != 0 || frame.payload.len() != 8 {
                    return Err(());
                }
                Ok(Some(raw_frame(&self.marker, PONG_TAG, 0, frame.payload)))
            }
            CALL_RAW_START_TAG => {
                if frame.stream == 0
                    || frame.payload.len() != 16
                    || self
                        .high_water_mark
                        .is_some_and(|previous| frame.stream <= previous)
                    || self.streams.len() == MAX_FALLBACK_LIVE_STREAMS
                {
                    return Err(());
                }
                self.high_water_mark = Some(frame.stream);
                self.streams.insert(frame.stream, [0; 6]);
                Ok(None)
            }
            CALL_ARGUMENT_TAG => {
                if frame.stream == 0 || !self.streams.contains_key(&frame.stream) {
                    return Err(());
                }
                if frame.payload.len() < 16 {
                    self.streams.remove(&frame.stream);
                    return Ok(Some(raw_failure(&self.marker, frame.stream)));
                }
                Ok(None)
            }
            CALL_ARGUMENTS_COMPLETE_TAG => {
                if frame.stream == 0 || !self.streams.contains_key(&frame.stream) {
                    return Err(());
                }
                if !frame.payload.is_empty() {
                    self.streams.remove(&frame.stream);
                    return Ok(Some(raw_failure(&self.marker, frame.stream)));
                }
                self.streams.remove(&frame.stream);
                Ok(Some(raw_failure(&self.marker, frame.stream)))
            }
            WINDOW_UPDATE_TAG => {
                if frame.stream == 0 || !self.streams.contains_key(&frame.stream) {
                    return Err(());
                }
                let payload: &[u8; 9] = frame.payload.try_into().map_err(|_| ())?;
                let channel = match payload[0] {
                    1..=6 => usize::from(payload[0] - 1),
                    _ => return Err(()),
                };
                let credit = u64::from_be_bytes(payload[1..].try_into().map_err(|_| ())?);
                if credit == 0 {
                    return Err(());
                }
                let windows = self
                    .streams
                    .get_mut(&frame.stream)
                    .expect("live stream checked");
                windows[channel] = windows[channel]
                    .checked_add(credit)
                    .filter(|value| *value <= MAX_CHANNEL_WINDOW)
                    .ok_or(())?;
                Ok(None)
            }
            CALL_CANCEL_TAG => {
                if frame.stream == 0 || !self.streams.contains_key(&frame.stream) {
                    return Err(());
                }
                if !frame.payload.is_empty() {
                    self.streams.remove(&frame.stream);
                    return Ok(Some(raw_failure(&self.marker, frame.stream)));
                }
                self.streams.remove(&frame.stream);
                Ok(Some(raw_frame(
                    &self.marker,
                    CALL_CANCELLED_TAG,
                    frame.stream,
                    &[],
                )))
            }
            _ => Err(()),
        }
    }
}

async fn serve_fallback(mut stream: UnixStream, marker: [u8; 4]) -> Result<(), SqliteSocketError> {
    let mut connection = FallbackConnection::new(marker);
    loop {
        let Some(encoded) = next_frame(&mut stream).await? else {
            return Ok(());
        };
        let frame = decode_raw_frame(&marker, &encoded)
            .map_err(|_| SqliteSocketError::protocol("invalid versioned frame"))?;
        let response = connection
            .handle(frame)
            .map_err(|_| SqliteSocketError::protocol("unsupported constructed call"))?;
        if let Some(response) = response {
            stream.write_all(&response).await.map_err(|source| {
                SqliteSocketError::io("could not write fallback response", source)
            })?;
        }
    }
}

fn raw_failure(marker: &[u8; 4], stream: u64) -> Vec<u8> {
    raw_frame(marker, CALL_FAILED_TAG, stream, &INTERNAL_FAILURE_WIRE)
}

fn raw_frame(marker: &[u8; 4], tag: u8, stream: u64, payload: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(FRAME_HEADER_LENGTH + payload.len());
    encoded.extend_from_slice(marker);
    encoded.push(tag);
    encoded.push(0);
    encoded.extend_from_slice(&stream.to_be_bytes());
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(payload);
    encoded
}
