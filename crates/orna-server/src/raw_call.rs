//! Fixed local client for one parameter-free raw recovery call.

use std::{
    fmt, fs,
    io::{self, Read, Write},
    mem,
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        io::{AsRawFd, FromRawFd},
        net::UnixStream,
    },
    sync::{
        Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use orna_core::FunctionId;
use orna_protocol::{
    CallFailure, ClientFrame, MAX_FRAME_PAYLOAD_LENGTH, RawCallClient, RawCallClientError,
    RawCallClientResponse, encode_client_frame, encode_value,
};

const RUNTIME_ROOT: &str = "/run/orna/default";
const SOCKET_PATH: &str = "/run/orna/default/orna.sock";
const CLIENT_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00";
const SERVER_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00";
const FRAME_HEADER_LENGTH: usize = 18;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);
const IO_POLL_INTERVAL: Duration = Duration::from_millis(100);

static INTERRUPTS: AtomicU8 = AtomicU8::new(0);
static RAW_CALL_OWNER: Mutex<()> = Mutex::new(());

/// The terminal public result of one local raw recovery call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalRawCallOutcome {
    /// The server completed the accepted call.
    Completed,
    /// The server returned one closed public failure.
    Failed(CallFailure),
    /// Local or server cancellation terminated the call.
    Cancelled,
}

/// A trusted local raw recovery client failure.
#[derive(Debug)]
#[non_exhaustive]
pub enum LocalRawCallError {
    /// The fixed runtime directory or public socket is unavailable or invalid.
    Connection,
    /// Protocol negotiation did not return the exact version-1 acknowledgement.
    Negotiation,
    /// A frame or response transition violates the closed protocol.
    Protocol {
        /// The protocol-owned validation failure, when available.
        source: Option<RawCallClientError>,
    },
    /// Standard output did not accept the complete canonical value stream.
    Output,
    /// The process could not install its interrupt boundary.
    Signal,
}

impl fmt::Display for LocalRawCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection => formatter.write_str("local raw-call connection failed"),
            Self::Negotiation => formatter.write_str("local raw-call negotiation failed"),
            Self::Protocol { .. } => formatter.write_str("local raw-call protocol failed"),
            Self::Output => formatter.write_str("local raw-call output failed"),
            Self::Signal => formatter.write_str("local raw-call signal handling failed"),
        }
    }
}

impl std::error::Error for LocalRawCallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Protocol {
                source: Some(source),
            } => Some(source),
            _ => None,
        }
    }
}

/// Runs one parameter-free protocol-1 call against the fixed local socket.
///
/// Complete canonical `ORV1` value envelopes are written to standard output in
/// event sequence. The server remains authoritative for peer authentication.
///
/// # Errors
///
/// Returns [`LocalRawCallError`] for invalid fixed socket metadata,
/// connection, negotiation, protocol, output, or signal-control failures.
pub fn run_local_raw_call(function: FunctionId) -> Result<LocalRawCallOutcome, LocalRawCallError> {
    let _owner = RAW_CALL_OWNER
        .try_lock()
        .map_err(|_| LocalRawCallError::Signal)?;
    let _interrupts = InterruptHandler::install()?;
    if let Err(error) = require_fixed_socket() {
        return if interruption_count() > 0 {
            Ok(LocalRawCallOutcome::Cancelled)
        } else {
            Err(error)
        };
    }
    let mut stream = match connect_interruptibly() {
        Err(_) if interruption_count() > 0 => return Ok(LocalRawCallOutcome::Cancelled),
        result => result?,
    };
    if stream.set_read_timeout(Some(IO_POLL_INTERVAL)).is_err()
        || stream.set_write_timeout(Some(IO_POLL_INTERVAL)).is_err()
    {
        return if interruption_count() > 0 {
            Ok(LocalRawCallOutcome::Cancelled)
        } else {
            Err(LocalRawCallError::Connection)
        };
    }
    match run_connected_raw_call(
        &mut stream,
        function,
        &mut StandardOutput {
            descriptor: nix::libc::STDOUT_FILENO,
        },
    ) {
        Err(LocalRawCallError::Signal) if interruption_count() > 0 => {
            Ok(LocalRawCallOutcome::Cancelled)
        }
        result => result,
    }
}

fn require_fixed_socket() -> Result<(), LocalRawCallError> {
    let runtime = fs::symlink_metadata(RUNTIME_ROOT).map_err(|_| LocalRawCallError::Connection)?;
    let socket = fs::symlink_metadata(SOCKET_PATH).map_err(|_| LocalRawCallError::Connection)?;
    if !runtime.file_type().is_dir() || !socket.file_type().is_socket() || socket.nlink() != 1 {
        return Err(LocalRawCallError::Connection);
    }
    Ok(())
}

fn connect_interruptibly() -> Result<UnixStream, LocalRawCallError> {
    // SAFETY: the fixed domain, type, and protocol form a valid socket request.
    let descriptor = unsafe {
        nix::libc::socket(
            nix::libc::AF_UNIX,
            nix::libc::SOCK_STREAM | nix::libc::SOCK_CLOEXEC | nix::libc::SOCK_NONBLOCK,
            0,
        )
    };
    if descriptor < 0 {
        return Err(LocalRawCallError::Connection);
    }
    // SAFETY: ownership of the newly created descriptor transfers exactly once.
    let stream = unsafe { UnixStream::from_raw_fd(descriptor) };
    // SAFETY: an all-zero Unix address is valid before its family and path are filled.
    let mut address: nix::libc::sockaddr_un = unsafe { mem::zeroed() };
    address.sun_family = nix::libc::AF_UNIX as nix::libc::sa_family_t;
    let path = SOCKET_PATH.as_bytes();
    if path.len() >= address.sun_path.len() {
        return Err(LocalRawCallError::Connection);
    }
    for (destination, source) in address.sun_path.iter_mut().zip(path) {
        *destination = *source as nix::libc::c_char;
    }
    // SAFETY: address points to a fully initialised sockaddr_un for the fixed path.
    let connected = unsafe {
        nix::libc::connect(
            stream.as_raw_fd(),
            std::ptr::from_ref(&address).cast::<nix::libc::sockaddr>(),
            mem::size_of::<nix::libc::sockaddr_un>() as nix::libc::socklen_t,
        )
    };
    if connected != 0 {
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(nix::libc::EINPROGRESS) | Some(nix::libc::EAGAIN)
        ) {
            return Err(LocalRawCallError::Connection);
        }
        wait_for_connection(&stream, Instant::now() + HANDSHAKE_TIMEOUT)?;
    }
    // SAFETY: F_GETFL and F_SETFL operate on the live owned socket descriptor.
    let flags = unsafe { nix::libc::fcntl(stream.as_raw_fd(), nix::libc::F_GETFL) };
    if flags < 0
        || unsafe {
            nix::libc::fcntl(
                stream.as_raw_fd(),
                nix::libc::F_SETFL,
                flags & !nix::libc::O_NONBLOCK,
            )
        } != 0
    {
        return Err(LocalRawCallError::Connection);
    }
    Ok(stream)
}

fn wait_for_connection(stream: &UnixStream, deadline: Instant) -> Result<(), LocalRawCallError> {
    loop {
        if interruption_count() > 0 {
            return Err(LocalRawCallError::Signal);
        }
        let mut descriptor = nix::libc::pollfd {
            fd: stream.as_raw_fd(),
            events: nix::libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialised pollfd for the call duration.
        let result =
            unsafe { nix::libc::poll(&mut descriptor, 1, IO_POLL_INTERVAL.as_millis() as i32) };
        if result > 0 {
            let mut socket_error = 0;
            let mut length = mem::size_of_val(&socket_error) as nix::libc::socklen_t;
            // SAFETY: both output pointers are valid and sized for SO_ERROR.
            if unsafe {
                nix::libc::getsockopt(
                    stream.as_raw_fd(),
                    nix::libc::SOL_SOCKET,
                    nix::libc::SO_ERROR,
                    std::ptr::from_mut(&mut socket_error).cast(),
                    &mut length,
                )
            } != 0
                || socket_error != 0
            {
                return Err(LocalRawCallError::Connection);
            }
            return Ok(());
        }
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(LocalRawCallError::Connection);
        }
        if Instant::now() >= deadline {
            return Err(LocalRawCallError::Connection);
        }
    }
}

enum OutputWriteError {
    Interrupted,
    Failed,
}

trait ValueOutput {
    fn write_value(
        &mut self,
        bytes: &[u8],
        interrupts_seen: &mut u8,
    ) -> Result<(), OutputWriteError>;
}

struct StandardOutput {
    descriptor: i32,
}

impl ValueOutput for StandardOutput {
    fn write_value(
        &mut self,
        bytes: &[u8],
        interrupts_seen: &mut u8,
    ) -> Result<(), OutputWriteError> {
        let deadline = Instant::now() + FRAME_TIMEOUT;
        let mut written = 0;
        let mut cancellation_pending = false;
        while written < bytes.len() {
            let observed = interruption_count();
            if observed != *interrupts_seen {
                *interrupts_seen = observed;
                if observed >= 2 {
                    return Err(OutputWriteError::Interrupted);
                }
                cancellation_pending = true;
            }
            let mut descriptor = nix::libc::pollfd {
                fd: self.descriptor,
                events: nix::libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: descriptor points to one initialised pollfd for the call duration.
            let result =
                unsafe { nix::libc::poll(&mut descriptor, 1, IO_POLL_INTERVAL.as_millis() as i32) };
            if result < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(OutputWriteError::Failed);
            }
            if result == 0 {
                if Instant::now() >= deadline {
                    return Err(OutputWriteError::Failed);
                }
                continue;
            }
            let length = (bytes.len() - written).min(4096);
            // SAFETY: the selected byte range is live and the descriptor is standard output.
            let result = unsafe {
                nix::libc::write(
                    self.descriptor,
                    bytes[written..written + length].as_ptr().cast(),
                    length,
                )
            };
            if result > 0 {
                written += result as usize;
            } else if result < 0 && retryable(&io::Error::last_os_error()) {
                continue;
            } else {
                return Err(OutputWriteError::Failed);
            }
        }
        if cancellation_pending {
            Err(OutputWriteError::Interrupted)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
impl<T: Write> ValueOutput for T {
    fn write_value(&mut self, bytes: &[u8], _: &mut u8) -> Result<(), OutputWriteError> {
        self.write_all(bytes).map_err(|_| OutputWriteError::Failed)
    }
}

fn run_connected_raw_call(
    stream: &mut UnixStream,
    function: FunctionId,
    output: &mut impl ValueOutput,
) -> Result<LocalRawCallOutcome, LocalRawCallError> {
    let mut interrupts_seen = 0;
    write_interruptibly(
        stream,
        &CLIENT_HELLO,
        Instant::now() + HANDSHAKE_TIMEOUT,
        &mut interrupts_seen,
        false,
    )?;
    let mut acknowledgement = [0_u8; SERVER_ACK.len()];
    read_exact_interruptibly(
        stream,
        &mut acknowledgement,
        Instant::now() + HANDSHAKE_TIMEOUT,
        &mut interrupts_seen,
        false,
    )?;
    if acknowledgement != SERVER_ACK {
        return Err(LocalRawCallError::Negotiation);
    }

    if interruption_count() != interrupts_seen {
        return Ok(LocalRawCallOutcome::Cancelled);
    }
    let (mut client, frames) = RawCallClient::start(function);
    let mut stream_created = false;
    let mut cancellation_sent = false;
    for frame in frames {
        let creates_stream = matches!(frame, ClientFrame::CallRawStart { .. });
        let encoded = encode_client_frame(&frame).map_err(protocol_codec_failure)?;
        write_interruptibly(
            stream,
            &encoded,
            Instant::now() + FRAME_TIMEOUT,
            &mut interrupts_seen,
            stream_created,
        )?;
        if creates_stream {
            stream_created = true;
        }
        send_pending_cancellation(
            stream,
            &mut client,
            &mut interrupts_seen,
            stream_created,
            &mut cancellation_sent,
        )?;
        if cancellation_sent {
            break;
        }
    }

    let mut reader = FrameReader::new();
    let mut output_failed = false;
    let mut deadline = Instant::now() + FRAME_TIMEOUT;
    let mut cancellation_deadline = cancellation_sent.then(|| Instant::now() + FRAME_TIMEOUT);
    loop {
        let cancellation_was_sent = cancellation_sent;
        send_pending_cancellation(
            stream,
            &mut client,
            &mut interrupts_seen,
            true,
            &mut cancellation_sent,
        )?;
        if !cancellation_was_sent && cancellation_sent {
            cancellation_deadline = Some(Instant::now() + FRAME_TIMEOUT);
        }
        if interrupts_seen >= 2 {
            return Ok(LocalRawCallOutcome::Cancelled);
        }
        let Some(encoded) = reader
            .poll(stream)
            .map_err(|_| LocalRawCallError::Protocol { source: None })?
        else {
            if Instant::now() >= cancellation_deadline.unwrap_or(deadline) {
                return Err(LocalRawCallError::Protocol { source: None });
            }
            continue;
        };
        if cancellation_deadline.is_none() {
            deadline = Instant::now() + FRAME_TIMEOUT;
        }
        match client
            .receive_encoded(&encoded)
            .map_err(|source| LocalRawCallError::Protocol {
                source: Some(source),
            })? {
            RawCallClientResponse::Accepted { .. } => {}
            RawCallClientResponse::Values(values) if output_failed => {
                drop(values);
            }
            RawCallClientResponse::Values(values) => {
                for value in values {
                    let encoded = encode_value(&value).map_err(protocol_value_failure)?;
                    match output.write_value(&encoded, &mut interrupts_seen) {
                        Ok(()) => {}
                        Err(OutputWriteError::Interrupted) if interrupts_seen >= 2 => {
                            return Ok(LocalRawCallOutcome::Cancelled);
                        }
                        Err(OutputWriteError::Interrupted) => {
                            request_cancellation(stream, &mut client, &mut interrupts_seen)?;
                            cancellation_sent = true;
                            cancellation_deadline = Some(Instant::now() + FRAME_TIMEOUT);
                            break;
                        }
                        Err(OutputWriteError::Failed) => {
                            output_failed = true;
                            match request_cancellation(stream, &mut client, &mut interrupts_seen) {
                                Ok(()) => {}
                                Err(LocalRawCallError::Signal) if interrupts_seen >= 2 => {
                                    return Ok(LocalRawCallOutcome::Cancelled);
                                }
                                Err(_) => return Err(LocalRawCallError::Output),
                            }
                            cancellation_sent = true;
                            cancellation_deadline = Some(Instant::now() + FRAME_TIMEOUT);
                            break;
                        }
                    }
                }
            }
            RawCallClientResponse::Completed => {
                return if output_failed {
                    Err(LocalRawCallError::Output)
                } else {
                    Ok(LocalRawCallOutcome::Completed)
                };
            }
            RawCallClientResponse::Failed(failure) => {
                return if output_failed {
                    Err(LocalRawCallError::Output)
                } else {
                    Ok(LocalRawCallOutcome::Failed(failure))
                };
            }
            RawCallClientResponse::Cancelled => {
                return if output_failed {
                    Err(LocalRawCallError::Output)
                } else {
                    Ok(LocalRawCallOutcome::Cancelled)
                };
            }
        }
    }
}

fn send_pending_cancellation(
    stream: &mut UnixStream,
    client: &mut RawCallClient,
    interrupts_seen: &mut u8,
    stream_created: bool,
    cancellation_sent: &mut bool,
) -> Result<(), LocalRawCallError> {
    let observed = interruption_count();
    if observed == *interrupts_seen {
        return Ok(());
    }
    if !stream_created {
        *interrupts_seen = observed;
        return Ok(());
    }
    if !*cancellation_sent {
        request_cancellation(stream, client, interrupts_seen)?;
        *cancellation_sent = true;
    }
    *interrupts_seen = observed;
    Ok(())
}

fn request_cancellation(
    stream: &mut UnixStream,
    client: &mut RawCallClient,
    interrupts_seen: &mut u8,
) -> Result<(), LocalRawCallError> {
    let frame = client
        .request_cancellation()
        .map_err(|source| LocalRawCallError::Protocol {
            source: Some(source),
        })?;
    let encoded = encode_client_frame(&frame).map_err(protocol_codec_failure)?;
    write_interruptibly(
        stream,
        &encoded,
        Instant::now() + FRAME_TIMEOUT,
        interrupts_seen,
        true,
    )
}

fn write_interruptibly(
    stream: &mut UnixStream,
    bytes: &[u8],
    deadline: Instant,
    interrupts_seen: &mut u8,
    stream_created: bool,
) -> Result<(), LocalRawCallError> {
    let mut written = 0;
    while written < bytes.len() {
        let observed = interruption_count();
        if observed != *interrupts_seen && (!stream_created || observed >= 2) {
            *interrupts_seen = observed;
            return Err(LocalRawCallError::Signal);
        }
        match stream.write(&bytes[written..]) {
            Ok(0) => return Err(LocalRawCallError::Connection),
            Ok(length) => written += length,
            Err(error) if retryable(&error) && Instant::now() < deadline => {}
            Err(_) => return Err(LocalRawCallError::Connection),
        }
    }
    Ok(())
}

fn read_exact_interruptibly(
    stream: &mut UnixStream,
    bytes: &mut [u8],
    deadline: Instant,
    interrupts_seen: &mut u8,
    stream_created: bool,
) -> Result<(), LocalRawCallError> {
    let mut read = 0;
    while read < bytes.len() {
        let observed = interruption_count();
        if observed != *interrupts_seen {
            *interrupts_seen = observed;
            if !stream_created || observed >= 2 {
                return Err(LocalRawCallError::Signal);
            }
        }
        match stream.read(&mut bytes[read..]) {
            Ok(0) => return Err(LocalRawCallError::Connection),
            Ok(length) => read += length,
            Err(error) if retryable(&error) && Instant::now() < deadline => {}
            Err(_) => return Err(LocalRawCallError::Connection),
        }
    }
    Ok(())
}

fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

struct FrameReader {
    header: [u8; FRAME_HEADER_LENGTH],
    header_length: usize,
    payload: Vec<u8>,
    payload_length: usize,
}

impl FrameReader {
    const fn new() -> Self {
        Self {
            header: [0; FRAME_HEADER_LENGTH],
            header_length: 0,
            payload: Vec::new(),
            payload_length: 0,
        }
    }

    fn poll(&mut self, stream: &mut UnixStream) -> io::Result<Option<Vec<u8>>> {
        if self.header_length < self.header.len() {
            match stream.read(&mut self.header[self.header_length..]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(length) => self.header_length += length,
                Err(error) if retryable(&error) => return Ok(None),
                Err(error) => return Err(error),
            }
            if self.header_length < self.header.len() {
                return Ok(None);
            }
            self.payload_length = u32::from_be_bytes(
                self.header[14..18]
                    .try_into()
                    .expect("fixed frame length slice"),
            ) as usize;
            if self.payload_length > MAX_FRAME_PAYLOAD_LENGTH {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            self.payload.resize(self.payload_length, 0);
        }
        while self.payload_length > 0 {
            let offset = self.payload.len() - self.payload_length;
            match stream.read(&mut self.payload[offset..]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(length) => self.payload_length -= length,
                Err(error) if retryable(&error) => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        let mut encoded = self.header.to_vec();
        encoded.append(&mut self.payload);
        *self = Self::new();
        Ok(Some(encoded))
    }
}

struct InterruptHandler {
    previous: SigAction,
}

impl InterruptHandler {
    fn install() -> Result<Self, LocalRawCallError> {
        INTERRUPTS.store(0, Ordering::SeqCst);
        let action = SigAction::new(
            SigHandler::Handler(record_interrupt),
            SaFlags::empty(),
            SigSet::empty(),
        );
        // SAFETY: the handler performs only one lock-free atomic update.
        let previous =
            unsafe { sigaction(Signal::SIGINT, &action) }.map_err(|_| LocalRawCallError::Signal)?;
        Ok(Self { previous })
    }
}

impl Drop for InterruptHandler {
    fn drop(&mut self) {
        // SAFETY: this restores the action returned by the successful installation.
        let _ = unsafe { sigaction(Signal::SIGINT, &self.previous) };
    }
}

extern "C" fn record_interrupt(_: i32) {
    let _ = INTERRUPTS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
        Some(count.saturating_add(1))
    });
}

fn interruption_count() -> u8 {
    INTERRUPTS.load(Ordering::SeqCst)
}

fn protocol_codec_failure(_: orna_protocol::FrameCodecError) -> LocalRawCallError {
    LocalRawCallError::Protocol { source: None }
}

fn protocol_value_failure(_: orna_protocol::ValueCodecError) -> LocalRawCallError {
    LocalRawCallError::Protocol { source: None }
}

#[cfg(test)]
mod tests {
    use std::{fs::File, os::unix::net::UnixStream, path::Path, sync::Mutex, thread};

    use orna_core::value::RuntimeValue;
    use orna_protocol::{
        Event, EventRecord, ServerFrame, decode_client_frame, encode_server_frame,
    };

    use super::*;

    static INTERRUPT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_interrupt_tests() -> std::sync::MutexGuard<'static, ()> {
        INTERRUPT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn connected_client_writes_exact_frames_and_canonical_values() {
        let _interrupt_guard = lock_interrupt_tests();
        let function = FunctionId::from_bytes([0x11; 16]);
        let (mut server, mut client_stream) = UnixStream::pair().unwrap();
        for stream in [&server, &client_stream] {
            stream.set_read_timeout(Some(IO_POLL_INTERVAL)).unwrap();
            stream.set_write_timeout(Some(IO_POLL_INTERVAL)).unwrap();
        }
        INTERRUPTS.store(0, Ordering::SeqCst);
        let server_task = thread::spawn(move || {
            let mut hello = [0; CLIENT_HELLO.len()];
            server.read_exact(&mut hello).unwrap();
            assert_eq!(hello, CLIENT_HELLO);
            server.write_all(&SERVER_ACK).unwrap();
            for expected in 0..3 {
                let encoded = read_test_frame(&mut server);
                let frame = decode_client_frame(&encoded).unwrap();
                match expected {
                    0 => assert_eq!(
                        frame,
                        ClientFrame::CallRawStart {
                            stream: 1,
                            function,
                        }
                    ),
                    1 => assert!(matches!(frame, ClientFrame::WindowUpdate { stream: 1, .. })),
                    _ => assert_eq!(frame, ClientFrame::CallArgumentsComplete { stream: 1 }),
                }
            }
            for frame in [
                ServerFrame::CallAccepted {
                    stream: 1,
                    invocation: orna_core::InvocationId::from_bytes([0x22; 16]),
                },
                ServerFrame::EventBatch {
                    stream: 1,
                    channel: orna_protocol::Channel::ResultValues,
                    events: vec![
                        EventRecord {
                            sequence: 1,
                            event: Event::Value(RuntimeValue::Boolean(true)),
                        },
                        EventRecord {
                            sequence: 2,
                            event: Event::Value(RuntimeValue::Integer(7)),
                        },
                    ],
                },
                ServerFrame::CallCompleted { stream: 1 },
            ] {
                server
                    .write_all(&encode_server_frame(&frame).unwrap())
                    .unwrap();
            }
        });
        let mut output = Vec::new();
        assert_eq!(
            run_connected_raw_call(&mut client_stream, function, &mut output).unwrap(),
            LocalRawCallOutcome::Completed
        );
        let mut expected = encode_value(&RuntimeValue::Boolean(true)).unwrap();
        expected.extend(encode_value(&RuntimeValue::Integer(7)).unwrap());
        assert_eq!(output, expected);
        server_task.join().unwrap();
    }

    fn read_test_frame(stream: &mut UnixStream) -> Vec<u8> {
        let mut header = [0; FRAME_HEADER_LENGTH];
        stream.read_exact(&mut header).unwrap();
        let length = u32::from_be_bytes(header[14..18].try_into().unwrap()) as usize;
        let mut encoded = header.to_vec();
        encoded.resize(FRAME_HEADER_LENGTH + length, 0);
        stream
            .read_exact(&mut encoded[FRAME_HEADER_LENGTH..])
            .unwrap();
        encoded
    }

    #[test]
    fn connected_client_preserves_failure_and_fragmented_frames() {
        let _interrupt_guard = lock_interrupt_tests();
        let function = FunctionId::from_bytes([0x11; 16]);
        let (mut server, mut client_stream) = UnixStream::pair().unwrap();
        for stream in [&server, &client_stream] {
            stream.set_read_timeout(Some(IO_POLL_INTERVAL)).unwrap();
            stream.set_write_timeout(Some(IO_POLL_INTERVAL)).unwrap();
        }
        INTERRUPTS.store(0, Ordering::SeqCst);
        let server_task = thread::spawn(move || {
            let mut hello = [0; CLIENT_HELLO.len()];
            server.read_exact(&mut hello).unwrap();
            for byte in SERVER_ACK {
                server.write_all(&[byte]).unwrap();
            }
            for _ in 0..3 {
                read_test_frame(&mut server);
            }
            for frame in [
                ServerFrame::CallAccepted {
                    stream: 1,
                    invocation: orna_core::InvocationId::from_bytes([0x22; 16]),
                },
                ServerFrame::CallFailed {
                    stream: 1,
                    failure: CallFailure::ExecuteDenied,
                },
            ] {
                for byte in encode_server_frame(&frame).unwrap() {
                    server.write_all(&[byte]).unwrap();
                }
            }
        });
        let mut output = Vec::new();
        assert_eq!(
            run_connected_raw_call(&mut client_stream, function, &mut output).unwrap(),
            LocalRawCallOutcome::Failed(CallFailure::ExecuteDenied)
        );
        assert!(output.is_empty());
        server_task.join().unwrap();
    }

    #[test]
    fn first_interrupt_after_stream_creation_sends_one_cancellation() {
        let _interrupt_guard = lock_interrupt_tests();
        let function = FunctionId::from_bytes([0x11; 16]);
        let (mut server, mut client_stream) = UnixStream::pair().unwrap();
        for stream in [&server, &client_stream] {
            stream.set_read_timeout(Some(IO_POLL_INTERVAL)).unwrap();
            stream.set_write_timeout(Some(IO_POLL_INTERVAL)).unwrap();
        }
        let (mut client, _) = RawCallClient::start(function);
        let mut interrupts_seen = 0;
        let mut cancellation_sent = false;
        INTERRUPTS.store(1, Ordering::SeqCst);
        send_pending_cancellation(
            &mut client_stream,
            &mut client,
            &mut interrupts_seen,
            true,
            &mut cancellation_sent,
        )
        .unwrap();
        assert_eq!(interrupts_seen, 1);
        assert!(cancellation_sent);
        assert_eq!(
            decode_client_frame(&read_test_frame(&mut server)).unwrap(),
            ClientFrame::CallCancel { stream: 1 }
        );
        send_pending_cancellation(
            &mut client_stream,
            &mut client,
            &mut interrupts_seen,
            true,
            &mut cancellation_sent,
        )
        .unwrap();
        let mut byte = [0];
        assert!(matches!(
            server.read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn first_output_interrupt_finishes_the_current_envelope() {
        let _interrupt_guard = lock_interrupt_tests();
        let mut descriptors = [0; 2];
        // SAFETY: descriptors points to two writable descriptor slots.
        assert_eq!(
            unsafe { nix::libc::pipe2(descriptors.as_mut_ptr(), nix::libc::O_CLOEXEC) },
            0
        );
        // SAFETY: each freshly created descriptor transfers to exactly one File.
        let mut input = unsafe { File::from_raw_fd(descriptors[0]) };
        // SAFETY: each freshly created descriptor transfers to exactly one File.
        let output = unsafe { File::from_raw_fd(descriptors[1]) };
        let envelope = encode_value(&RuntimeValue::Boolean(true)).unwrap();
        let mut interrupts_seen = 0;
        INTERRUPTS.store(1, Ordering::SeqCst);
        assert!(matches!(
            StandardOutput {
                descriptor: output.as_raw_fd(),
            }
            .write_value(&envelope, &mut interrupts_seen),
            Err(OutputWriteError::Interrupted)
        ));
        drop(output);
        let mut actual = Vec::new();
        input.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, envelope);
        assert_eq!(interrupts_seen, 1);
    }

    struct FailingOutput;

    impl Write for FailingOutput {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn output_failure_cancels_and_drains_before_failing() {
        let _interrupt_guard = lock_interrupt_tests();
        let function = FunctionId::from_bytes([0x11; 16]);
        let (mut server, mut client_stream) = UnixStream::pair().unwrap();
        for stream in [&server, &client_stream] {
            stream.set_read_timeout(Some(IO_POLL_INTERVAL)).unwrap();
            stream.set_write_timeout(Some(IO_POLL_INTERVAL)).unwrap();
        }
        INTERRUPTS.store(0, Ordering::SeqCst);
        let server_task = thread::spawn(move || {
            let mut hello = [0; CLIENT_HELLO.len()];
            server.read_exact(&mut hello).unwrap();
            server.write_all(&SERVER_ACK).unwrap();
            for _ in 0..3 {
                read_test_frame(&mut server);
            }
            for frame in [
                ServerFrame::CallAccepted {
                    stream: 1,
                    invocation: orna_core::InvocationId::from_bytes([0x22; 16]),
                },
                ServerFrame::EventBatch {
                    stream: 1,
                    channel: orna_protocol::Channel::ResultValues,
                    events: vec![EventRecord {
                        sequence: 1,
                        event: Event::Value(RuntimeValue::Boolean(true)),
                    }],
                },
            ] {
                server
                    .write_all(&encode_server_frame(&frame).unwrap())
                    .unwrap();
            }
            assert_eq!(
                decode_client_frame(&read_test_frame(&mut server)).unwrap(),
                ClientFrame::CallCancel { stream: 1 }
            );
            server
                .write_all(&encode_server_frame(&ServerFrame::CallCancelled { stream: 1 }).unwrap())
                .unwrap();
        });
        assert!(matches!(
            run_connected_raw_call(&mut client_stream, function, &mut FailingOutput),
            Err(LocalRawCallError::Output)
        ));
        server_task.join().unwrap();
    }

    #[test]
    fn fixed_socket_metadata_rejects_absent_authority() {
        if !Path::new(RUNTIME_ROOT).exists() {
            assert!(matches!(
                require_fixed_socket(),
                Err(LocalRawCallError::Connection)
            ));
        }
    }
}
