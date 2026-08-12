//! Authenticated local raw-call connection handling.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    future::Future,
    io::{self, Write},
    os::unix::net::UnixStream as StandardUnixStream,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use orna_core::security::AuthenticatedSession;
use orna_kernel_postgres::PostgresKernel;
use orna_protocol::{
    ClientAction, ClientFrame, ConnectionError, FrameCodecError, MAX_FRAME_PAYLOAD_LENGTH,
    ProtocolConnection, RawCall, ServerAction, ServerFrame, decode_client_frame,
    encode_server_frame,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    task::{JoinError, JoinSet},
    time::{Instant, timeout_at},
};

use crate::{RawClientDispatch, authenticate_local_stream};

const CLIENT_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00";
const SERVER_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00";
const FRAME_HEADER_LENGTH: usize = 18;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const SHARED_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;
const KERNEL_OPERATION_LIMIT: usize = 64;

/// Listener-wide admission resources for authenticated local raw calls.
#[derive(Clone)]
pub struct LocalRawSocketResources {
    payload: Arc<Semaphore>,
    kernel_operations: Arc<Semaphore>,
}

impl LocalRawSocketResources {
    /// Creates the fixed version-one listener budgets.
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
/// Returns [`LocalRawSocketError`] for handshake, authentication, capacity,
/// I/O, codec, state-machine, or protected task failures. No error text is
/// written to the client.
pub async fn serve_local_raw_stream(
    kernel: PostgresKernel,
    stream: StandardUnixStream,
    resources: LocalRawSocketResources,
) -> Result<(), LocalRawSocketError> {
    run_owned_connection(async move { negotiate_and_drive(kernel, stream, resources).await }).await
}

async fn run_owned_connection<F>(connection: F) -> Result<(), LocalRawSocketError>
where
    F: Future<Output = Result<(), LocalRawSocketError>> + Send + 'static,
{
    tokio::spawn(connection)
        .await
        .map_err(|source| LocalRawSocketError::ConnectionTask { source })?
}

async fn negotiate_and_drive(
    kernel: PostgresKernel,
    stream: StandardUnixStream,
    resources: LocalRawSocketResources,
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
    read_exact_before(
        &mut stream,
        &mut hello,
        Instant::now() + HANDSHAKE_TIMEOUT,
        LocalRawSocketError::HandshakeTimeout,
    )
    .await?;
    if hello != CLIENT_HELLO {
        return Err(LocalRawSocketError::InvalidHello);
    }

    let authentication_permit = resources.reserve_kernel_operation()?;
    let session = authenticate_local_stream(&kernel, &peer_stream)
        .await
        .map_err(|source| LocalRawSocketError::Authentication { source })?;
    drop(authentication_permit);
    stream
        .write_all(&SERVER_ACK)
        .await
        .map_err(|source| LocalRawSocketError::Io { source })?;

    drive_authenticated_stream(RawDispatchService { kernel }, session, stream, resources).await
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

async fn drive_authenticated_stream<D: DispatchService>(
    dispatcher: D,
    session: AuthenticatedSession,
    stream: UnixStream,
    resources: LocalRawSocketResources,
) -> Result<(), LocalRawSocketError> {
    let (reader, mut writer) = stream.into_split();
    let (frame_sender, mut frame_receiver) = mpsc::channel(64);
    let reader_task = spawn_frame_reader(reader, resources.clone(), frame_sender);
    let mut connection = ProtocolConnection::new();
    let mut retained_payload = BTreeMap::<u64, Vec<PayloadReservation>>::new();
    let mut cancelled = BTreeSet::<u64>::new();
    let mut pending = BTreeMap::<u64, DispatchCompletion>::new();
    let mut tasks = JoinSet::<(u64, DispatchCompletion)>::new();
    let mut unstarted = VecDeque::<UnstartedDispatch>::new();
    let result = loop {
        if let Err(error) = flush_pending(&mut connection, &mut pending, &mut writer).await {
            break Err(error);
        }

        enum Next {
            Frame(Result<Option<IncomingFrame>, LocalRawSocketError>),
            Completion(Option<Result<(u64, DispatchCompletion), JoinError>>),
            Start,
        }

        let next = if let Some(dispatch) = unstarted.front_mut() {
            if dispatch.defer_once {
                dispatch.defer_once = false;
                tokio::select! {
                    biased;
                    frame = frame_receiver.recv() => Next::Frame(frame.unwrap_or(Ok(None))),
                    () = tokio::task::yield_now() => Next::Start,
                }
            } else {
                Next::Start
            }
        } else if tasks.is_empty() {
            Next::Frame(frame_receiver.recv().await.unwrap_or(Ok(None)))
        } else {
            tokio::select! {
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
                if let Err(error) = handle_client_frame(
                    incoming,
                    &dispatcher,
                    &session,
                    &resources,
                    &mut connection,
                    &mut retained_payload,
                    &mut cancelled,
                    &mut pending,
                    &mut unstarted,
                    &mut writer,
                )
                .await
                {
                    break Err(error);
                }
            }
            Next::Frame(Ok(None)) => break Ok(()),
            Next::Frame(Err(error)) => break Err(error),
            Next::Completion(Some(Ok((stream_id, completion)))) => {
                let completion = if cancelled.remove(&stream_id) {
                    DispatchCompletion {
                        actions: VecDeque::from([completion.cancellation.clone()]),
                        cancellation: completion.cancellation,
                    }
                } else {
                    completion
                };
                pending.insert(stream_id, completion);
            }
            Next::Completion(Some(Err(source))) => {
                break Err(LocalRawSocketError::DispatchTask { source });
            }
            Next::Completion(None) => {}
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
    resources: &LocalRawSocketResources,
    connection: &mut ProtocolConnection,
    retained_payload: &mut BTreeMap<u64, Vec<PayloadReservation>>,
    cancelled: &mut BTreeSet<u64>,
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    unstarted: &mut VecDeque<UnstartedDispatch>,
    socket: &mut OwnedWriteHalf,
) -> Result<(), LocalRawSocketError> {
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
    let action = connection
        .receive(incoming.frame)
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
            let frame = connection
                .apply(accepted)
                .map_err(|source| LocalRawSocketError::Connection { source })?;
            write_server_frame(socket, &frame).await?;
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
            write_server_frame(socket, &frame).await?;
        }
        None => {}
    }
    Ok(())
}

fn start_one_dispatch(
    unstarted: &mut VecDeque<UnstartedDispatch>,
    tasks: &mut JoinSet<(u64, DispatchCompletion)>,
) {
    let dispatch = unstarted.pop_front().expect("unstarted dispatch exists");
    tasks.spawn(async move {
        let completion = dispatch.future.await;
        drop(dispatch.guards);
        (dispatch.stream, completion)
    });
}

async fn flush_pending(
    connection: &mut ProtocolConnection,
    pending: &mut BTreeMap<u64, DispatchCompletion>,
    stream: &mut OwnedWriteHalf,
) -> Result<(), LocalRawSocketError> {
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
            let frame = match connection.apply(action) {
                Ok(frame) => frame,
                Err(ConnectionError::InsufficientCredit { .. }) => break,
                Err(source) => return Err(LocalRawSocketError::Connection { source }),
            };
            write_server_frame(stream, &frame).await?;
            pending
                .get_mut(&stream_id)
                .expect("pending completion exists")
                .actions
                .pop_front();
        }
    }
    Ok(())
}

fn spawn_frame_reader(
    mut reader: OwnedReadHalf,
    resources: LocalRawSocketResources,
    sender: mpsc::Sender<Result<Option<IncomingFrame>, LocalRawSocketError>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let frame =
                read_client_frame(&mut reader, &resources, Instant::now() + FRAME_IDLE_TIMEOUT)
                    .await;
            let terminal = !matches!(frame, Ok(Some(_)));
            if sender.send(frame).await.is_err() || terminal {
                return;
            }
        }
    })
}

async fn read_client_frame<R: AsyncRead + Unpin>(
    stream: &mut R,
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
    let frame =
        decode_client_frame(&encoded).map_err(|source| LocalRawSocketError::Frame { source })?;
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
    stream: &mut OwnedWriteHalf,
    frame: &ServerFrame,
) -> Result<(), LocalRawSocketError> {
    let encoded =
        encode_server_frame(frame).map_err(|source| LocalRawSocketError::Frame { source })?;
    stream
        .write_all(&encoded)
        .await
        .map_err(|source| LocalRawSocketError::Io { source })
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
        future::poll_fn,
        str::FromStr,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        task::Poll,
    };

    use orna_core::{
        CatalogueRevisionId, FunctionId, InvocationId, PrincipalId, SourceRevisionId,
        revision::RevisionPair,
        security::{
            AuthenticatedSession, Principal, PrincipalKind, PrincipalStatus, SecuritySnapshot,
        },
        value::RuntimeValue,
    };
    use orna_protocol::{
        Channel, ClientFrame, Event, ServerFrame, decode_server_frame, encode_client_frame,
    };
    use tokio::sync::Notify;
    use tokio::time::timeout;

    use super::*;

    const FUNCTION: FunctionId = FunctionId::from_bytes([1; 16]);

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
                    }
                }),
            }
        }
    }

    #[test]
    fn handshake_bytes_and_listener_budgets_are_exact() {
        assert_eq!(CLIENT_HELLO, *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00");
        assert_eq!(SERVER_ACK, *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00");

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
    async fn aborting_the_public_waiter_does_not_cancel_owned_connection_work() {
        let resources = LocalRawSocketResources::new();
        let gated = GatedDispatch::new();
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let completed = Arc::new(Notify::new());
        let completion_witness = Arc::clone(&completed);
        let owned_resources = resources.clone();
        let owned_dispatch = gated.clone();
        let owned = run_owned_connection(async move {
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
        let mut header = [0_u8; FRAME_HEADER_LENGTH];
        timeout(Duration::from_secs(1), stream.read_exact(&mut header))
            .await
            .expect("server frame timeout")
            .expect("server frame header");
        let length = u32::from_be_bytes(header[14..18].try_into().expect("fixed header")) as usize;
        let mut encoded = header.to_vec();
        encoded.resize(FRAME_HEADER_LENGTH + length, 0);
        stream
            .read_exact(&mut encoded[FRAME_HEADER_LENGTH..])
            .await
            .expect("server frame payload");
        decode_server_frame(&encoded).expect("server frame decodes")
    }
}
