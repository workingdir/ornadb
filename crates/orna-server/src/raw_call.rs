//! Fixed local client for one bounded raw recovery call.

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
use orna_core::{FunctionId, ParameterId, value::RuntimeValue};
use orna_protocol::{
    CallFailure, ClientFrame, MAX_FRAME_PAYLOAD_LENGTH, RawCallClient, RawCallClientError,
    RawCallClientResponse, decode_value, encode_client_frame, encode_value,
};

const RUNTIME_ROOT: &str = "/run/orna/default";
const SOCKET_PATH: &str = "/run/orna/default/orna.sock";
const CLIENT_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00";
const SERVER_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00";
const FRAME_HEADER_LENGTH: usize = 18;
const VALUE_HEADER_LENGTH: usize = 25;
const MAX_ARGUMENT_VALUE_LENGTH: usize = MAX_FRAME_PAYLOAD_LENGTH - 16;
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
    /// Standard input is not one complete bounded canonical ORV1 value.
    Input,
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
            Self::Input => formatter.write_str("orna: raw-call argument input is invalid"),
            Self::Connection => formatter.write_str("local raw-call connection failed"),
            Self::Negotiation => formatter.write_str("local raw-call negotiation failed"),
            Self::Protocol { .. } => formatter.write_str("local raw-call protocol failed"),
            Self::Output => formatter.write_str("local raw-call output failed"),
            Self::Signal => formatter.write_str("local raw-call signal handling failed"),
        }
    }
}

fn read_raw_call_envelope(
    input: &mut impl Read,
    interrupts_seen: u8,
    maximum_encoded_length: usize,
) -> Result<(RuntimeValue, usize), LocalRawCallError> {
    let mut encoded = vec![0; VALUE_HEADER_LENGTH];
    read_argument_input_exact(input, &mut encoded, interrupts_seen)?;

    let payload_length = u32::from_be_bytes(
        encoded[VALUE_HEADER_LENGTH - 4..VALUE_HEADER_LENGTH]
            .try_into()
            .expect("value header length is fixed"),
    ) as usize;
    let encoded_length = VALUE_HEADER_LENGTH
        .checked_add(payload_length)
        .filter(|length| *length <= MAX_ARGUMENT_VALUE_LENGTH && *length <= maximum_encoded_length)
        .ok_or(LocalRawCallError::Input)?;
    encoded.resize(encoded_length, 0);
    read_argument_input_exact(input, &mut encoded[VALUE_HEADER_LENGTH..], interrupts_seen)?;

    let value = decode_value(&encoded).map_err(|_| LocalRawCallError::Input)?;
    if interruption_count() != interrupts_seen {
        return Err(LocalRawCallError::Signal);
    }
    Ok((value, encoded_length))
}

fn require_argument_input_eof(
    input: &mut impl Read,
    interrupts_seen: u8,
) -> Result<(), LocalRawCallError> {
    let mut trailing = [0];
    loop {
        if interruption_count() != interrupts_seen {
            return Err(LocalRawCallError::Signal);
        }
        let result = input.read(&mut trailing);
        if interruption_count() != interrupts_seen {
            return Err(LocalRawCallError::Signal);
        }
        match result {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(LocalRawCallError::Input),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(LocalRawCallError::Input),
        }
    }
}

fn read_raw_call_argument(input: &mut impl Read) -> Result<RuntimeValue, LocalRawCallError> {
    let interrupts_seen = interruption_count();
    let (value, _) = read_raw_call_envelope(input, interrupts_seen, MAX_ARGUMENT_VALUE_LENGTH)?;
    require_argument_input_eof(input, interrupts_seen)?;
    Ok(value)
}

fn read_raw_call_argument_pair(
    input: &mut impl Read,
) -> Result<[RuntimeValue; 2], LocalRawCallError> {
    let interrupts_seen = interruption_count();
    let (first, first_length) =
        read_raw_call_envelope(input, interrupts_seen, MAX_ARGUMENT_VALUE_LENGTH)?;
    let second_maximum = MAX_FRAME_PAYLOAD_LENGTH
        .checked_sub(16)
        .and_then(|remaining| remaining.checked_sub(first_length))
        .and_then(|remaining| remaining.checked_sub(16))
        .ok_or(LocalRawCallError::Input)?;
    let (second, _) = read_raw_call_envelope(input, interrupts_seen, second_maximum)?;
    require_argument_input_eof(input, interrupts_seen)?;
    Ok([first, second])
}

fn read_argument_input_exact(
    input: &mut impl Read,
    bytes: &mut [u8],
    interrupts_seen: u8,
) -> Result<(), LocalRawCallError> {
    let mut read = 0;
    while read < bytes.len() {
        if interruption_count() != interrupts_seen {
            return Err(LocalRawCallError::Signal);
        }
        let result = input.read(&mut bytes[read..]);
        if interruption_count() != interrupts_seen {
            return Err(LocalRawCallError::Signal);
        }
        match result {
            Ok(0) => return Err(LocalRawCallError::Input),
            Ok(length) => read += length,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(LocalRawCallError::Input),
        }
    }
    Ok(())
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
    run_local_raw_call_after_input(function, RawCallArguments::None)
}

/// Runs one protocol-1 call with one canonical standard-input argument.
///
/// The complete bounded `ORV1` argument is read and validated before the
/// client checks or connects to the fixed local socket.
///
/// # Errors
///
/// Returns [`LocalRawCallError::Input`] when standard input is not exactly one
/// canonical bounded `ORV1` value. It returns the other [`LocalRawCallError`]
/// variants for fixed-socket, negotiation, protocol, output, or signal
/// failures.
pub fn run_local_raw_call_with_argument(
    function: FunctionId,
    parameter: ParameterId,
) -> Result<LocalRawCallOutcome, LocalRawCallError> {
    run_local_raw_call_with_argument_input(
        function,
        parameter,
        &mut StandardInput {
            descriptor: nix::libc::STDIN_FILENO,
        },
    )
}

/// Runs one protocol-1 call with one ordered pair of standard-input arguments.
///
/// Standard input must contain exactly two complete bounded `ORV1` envelopes
/// followed by EOF. The first value belongs to `first_parameter` and the
/// second belongs to `second_parameter`. Both values and their aggregate
/// retained protocol bytes are validated before the fixed socket is checked.
///
/// # Errors
///
/// Returns [`LocalRawCallError::Input`] when the parameters are equal or the
/// input is not exactly one canonical bounded argument pair. It returns the
/// other [`LocalRawCallError`] variants for fixed-socket, negotiation,
/// protocol, output, or signal failures.
pub fn run_local_raw_call_with_argument_pair(
    function: FunctionId,
    first_parameter: ParameterId,
    second_parameter: ParameterId,
) -> Result<LocalRawCallOutcome, LocalRawCallError> {
    run_local_raw_call_with_argument_pair_input(
        function,
        first_parameter,
        second_parameter,
        &mut StandardInput {
            descriptor: nix::libc::STDIN_FILENO,
        },
    )
}

fn run_local_raw_call_with_argument_input(
    function: FunctionId,
    parameter: ParameterId,
    input: &mut impl Read,
) -> Result<LocalRawCallOutcome, LocalRawCallError> {
    let _owner = RAW_CALL_OWNER
        .try_lock()
        .map_err(|_| LocalRawCallError::Signal)?;
    let _interrupts = InterruptHandler::install()?;
    let value = read_raw_call_argument(input);
    if interruption_count() > 0 {
        return Ok(LocalRawCallOutcome::Cancelled);
    }
    let value = value?;
    run_local_raw_call_after_input(function, RawCallArguments::One(parameter, value))
}

fn run_local_raw_call_with_argument_pair_input(
    function: FunctionId,
    first_parameter: ParameterId,
    second_parameter: ParameterId,
    input: &mut impl Read,
) -> Result<LocalRawCallOutcome, LocalRawCallError> {
    if first_parameter == second_parameter {
        return Err(LocalRawCallError::Input);
    }
    let _owner = RAW_CALL_OWNER
        .try_lock()
        .map_err(|_| LocalRawCallError::Signal)?;
    let _interrupts = InterruptHandler::install()?;
    let values = read_raw_call_argument_pair(input);
    if interruption_count() > 0 {
        return Ok(LocalRawCallOutcome::Cancelled);
    }
    let [first_value, second_value] = values?;
    run_local_raw_call_after_input(
        function,
        RawCallArguments::Pair(
            (first_parameter, first_value),
            (second_parameter, second_value),
        ),
    )
}

enum RawCallArguments {
    None,
    One(ParameterId, RuntimeValue),
    Pair((ParameterId, RuntimeValue), (ParameterId, RuntimeValue)),
}

fn run_local_raw_call_after_input(
    function: FunctionId,
    arguments: RawCallArguments,
) -> Result<LocalRawCallOutcome, LocalRawCallError> {
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
    let mut output = StandardOutput {
        descriptor: nix::libc::STDOUT_FILENO,
    };
    let result = match arguments {
        RawCallArguments::None => run_connected_raw_call(&mut stream, function, &mut output),
        RawCallArguments::One(parameter, value) => run_connected_raw_call_with_argument(
            &mut stream,
            function,
            parameter,
            value,
            &mut output,
        ),
        pair @ RawCallArguments::Pair(_, _) => {
            run_connected_raw_call_with_arguments(&mut stream, function, pair, &mut output)
        }
    };
    match result {
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

struct StandardInput {
    descriptor: i32,
}

impl Read for StandardInput {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            if interruption_count() > 0 {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            let mut descriptor = nix::libc::pollfd {
                fd: self.descriptor,
                events: nix::libc::POLLIN,
                revents: 0,
            };
            // SAFETY: descriptor points to one initialised pollfd for the call duration.
            let result =
                unsafe { nix::libc::poll(&mut descriptor, 1, IO_POLL_INTERVAL.as_millis() as i32) };
            if result == 0 {
                continue;
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                if retryable(&error) {
                    continue;
                }
                return Err(error);
            }
            // SAFETY: the destination is a live mutable byte slice and standard input
            // remains process-owned for the complete call.
            let result =
                unsafe { nix::libc::read(self.descriptor, bytes.as_mut_ptr().cast(), bytes.len()) };
            if result >= 0 {
                return Ok(result as usize);
            }
            let error = io::Error::last_os_error();
            if retryable(&error) {
                continue;
            }
            return Err(error);
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
    run_connected_raw_call_with_arguments(stream, function, RawCallArguments::None, output)
}

fn run_connected_raw_call_with_argument(
    stream: &mut UnixStream,
    function: FunctionId,
    parameter: ParameterId,
    value: RuntimeValue,
    output: &mut impl ValueOutput,
) -> Result<LocalRawCallOutcome, LocalRawCallError> {
    run_connected_raw_call_with_arguments(
        stream,
        function,
        RawCallArguments::One(parameter, value),
        output,
    )
}

fn run_connected_raw_call_with_arguments(
    stream: &mut UnixStream,
    function: FunctionId,
    arguments: RawCallArguments,
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
    let (mut client, [start, window, complete]) = RawCallClient::start(function);
    let arguments = match arguments {
        RawCallArguments::None => [None, None],
        RawCallArguments::One(parameter, value) => [
            Some(ClientFrame::CallArgument {
                stream: 1,
                parameter,
                value,
            }),
            None,
        ],
        RawCallArguments::Pair(
            (first_parameter, first_value),
            (second_parameter, second_value),
        ) => [
            Some(ClientFrame::CallArgument {
                stream: 1,
                parameter: first_parameter,
                value: first_value,
            }),
            Some(ClientFrame::CallArgument {
                stream: 1,
                parameter: second_parameter,
                value: second_value,
            }),
        ],
    };
    let mut stream_created = false;
    let mut cancellation_sent = false;
    for frame in [start, window]
        .into_iter()
        .chain(arguments.into_iter().flatten())
        .chain([complete])
    {
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
    use std::{
        error::Error as _, fs::File, io::Cursor, os::unix::net::UnixStream, path::Path,
        sync::Mutex, thread,
    };

    use orna_core::{ParameterId, value::RuntimeValue};
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

    #[test]
    fn connected_client_writes_argument_frames_and_preserves_the_response() {
        let _interrupt_guard = lock_interrupt_tests();
        let function = FunctionId::from_bytes([0x11; 16]);
        let parameter = ParameterId::from_bytes([0x33; 16]);
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
            for expected in 0..4 {
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
                    2 => assert_eq!(
                        frame,
                        ClientFrame::CallArgument {
                            stream: 1,
                            parameter,
                            value: RuntimeValue::Boolean(true),
                        }
                    ),
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
                    events: vec![EventRecord {
                        sequence: 1,
                        event: Event::Value(RuntimeValue::Boolean(true)),
                    }],
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
            run_connected_raw_call_with_argument(
                &mut client_stream,
                function,
                parameter,
                RuntimeValue::Boolean(true),
                &mut output
            )
            .unwrap(),
            LocalRawCallOutcome::Completed
        );
        let expected = encode_value(&RuntimeValue::Boolean(true)).unwrap();
        assert_eq!(output, expected);
        server_task.join().unwrap();
    }

    #[test]
    fn connected_client_writes_two_argument_frames_in_command_token_order() {
        let _interrupt_guard = lock_interrupt_tests();
        let function = FunctionId::from_bytes([0x11; 16]);
        let first_parameter = ParameterId::from_bytes([0x33; 16]);
        let second_parameter = ParameterId::from_bytes([0x44; 16]);
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
            for expected in 0..5 {
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
                    2 => assert_eq!(
                        frame,
                        ClientFrame::CallArgument {
                            stream: 1,
                            parameter: first_parameter,
                            value: RuntimeValue::Boolean(true),
                        }
                    ),
                    3 => assert_eq!(
                        frame,
                        ClientFrame::CallArgument {
                            stream: 1,
                            parameter: second_parameter,
                            value: RuntimeValue::Integer(7),
                        }
                    ),
                    _ => assert_eq!(frame, ClientFrame::CallArgumentsComplete { stream: 1 }),
                }
            }
            for frame in [
                ServerFrame::CallAccepted {
                    stream: 1,
                    invocation: orna_core::InvocationId::from_bytes([0x22; 16]),
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
            run_connected_raw_call_with_arguments(
                &mut client_stream,
                function,
                RawCallArguments::Pair(
                    (first_parameter, RuntimeValue::Boolean(true)),
                    (second_parameter, RuntimeValue::Integer(7)),
                ),
                &mut output,
            )
            .unwrap(),
            LocalRawCallOutcome::Completed
        );
        assert!(output.is_empty());
        server_task.join().unwrap();
    }

    #[test]
    fn raw_call_argument_input_requires_one_complete_orv1_envelope() {
        let _interrupt_guard = lock_interrupt_tests();
        INTERRUPTS.store(0, Ordering::SeqCst);
        let exact = [
            0x4f, 0x52, 0x56, 0x31, // ORV1 marker
            0x02, // Boolean tag
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Boolean type identity
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // Boolean type identity
            0x00, 0x00, 0x00, 0x01, // payload length
            0x01, // Boolean true payload
        ];
        assert_eq!(
            read_raw_call_argument(&mut Cursor::new(exact)).unwrap(),
            RuntimeValue::Boolean(true)
        );
        let mut trailing = exact.to_vec();
        trailing.push(0xaa);
        let error = read_raw_call_argument(&mut Cursor::new(trailing))
            .expect_err("one trailing byte after a complete envelope must reject the input");
        assert!(matches!(error, LocalRawCallError::Input));
        assert_eq!(
            error.to_string(),
            "orna: raw-call argument input is invalid"
        );
        assert!(error.source().is_none());
    }

    #[test]
    fn raw_call_argument_pair_input_decodes_two_concatenated_orv1_envelopes_in_order() {
        let _interrupt_guard = lock_interrupt_tests();
        INTERRUPTS.store(0, Ordering::SeqCst);
        let mut input = encode_value(&RuntimeValue::Boolean(true)).unwrap();
        input.extend(encode_value(&RuntimeValue::Integer(7)).unwrap());
        assert_eq!(
            read_raw_call_argument_pair(&mut Cursor::new(input)).unwrap(),
            [RuntimeValue::Boolean(true), RuntimeValue::Integer(7)]
        );
    }

    #[test]
    fn raw_call_argument_pair_input_rejects_malformed_truncated_and_trailing_boundaries() {
        let _interrupt_guard = lock_interrupt_tests();
        INTERRUPTS.store(0, Ordering::SeqCst);
        let first = encode_value(&RuntimeValue::Boolean(true)).unwrap();
        let second = encode_value(&RuntimeValue::Integer(7)).unwrap();
        let mut malformed = first.clone();
        malformed.extend([b'O', b'R', b'V', b'2']);
        let mut truncated = first.clone();
        truncated.extend(&second[..second.len() - 1]);
        let mut trailing = first.clone();
        trailing.extend(&second);
        trailing.push(0xaa);
        for input in [Vec::new(), first.clone(), malformed, truncated, trailing] {
            require_raw_call_argument_input(
                read_raw_call_argument_pair(&mut Cursor::new(input))
                    .expect_err("invalid pair input must be rejected"),
            );
        }
    }

    #[test]
    fn raw_call_argument_pair_input_rejects_the_retained_argument_aggregate_limit() {
        let _interrupt_guard = lock_interrupt_tests();
        INTERRUPTS.store(0, Ordering::SeqCst);
        let first = encode_value(&RuntimeValue::Boolean(true)).unwrap();
        let mut second_header = encode_value(&RuntimeValue::Boolean(true)).unwrap();
        second_header.truncate(VALUE_HEADER_LENGTH);
        second_header[VALUE_HEADER_LENGTH - 4..].copy_from_slice(
            &((MAX_ARGUMENT_VALUE_LENGTH - VALUE_HEADER_LENGTH) as u32).to_be_bytes(),
        );
        let mut bytes = first;
        bytes.extend(second_header);
        let mut input = PairAggregateHeaderOnlyReader::new(&bytes, bytes.len());
        require_raw_call_argument_input(
            read_raw_call_argument_pair(&mut input)
                .expect_err("the pair retained bytes must be bounded before a second payload read"),
        );
        assert!(!input.payload_requested);
    }

    #[test]
    fn raw_call_argument_input_validation_precedes_the_fixed_socket() {
        let _interrupt_guard = lock_interrupt_tests();
        INTERRUPTS.store(0, Ordering::SeqCst);
        // In an ordinary unit-test environment the fixed runtime socket is
        // absent. If it is unexpectedly present, skip this environmental
        // precedence tracer rather than connect to it.
        if require_fixed_socket().is_ok() {
            return;
        }
        assert!(
            matches!(require_fixed_socket(), Err(LocalRawCallError::Connection)),
            "the fixed socket boundary must be the connection failure in this environment"
        );
        let mut input = Cursor::new(vec![
            0x4f, 0x52, 0x56, 0x31, // ORV1 marker
            0x02, // Boolean tag
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Boolean type identity
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // Boolean type identity
            0x00, 0x00, 0x00, 0x01, // payload length
            0x01, // Boolean true payload
            0xaa, // trailing byte
        ]);
        let error = run_local_raw_call_with_argument_input(
            FunctionId::from_bytes([0x11; 16]),
            ParameterId::from_bytes([0x33; 16]),
            &mut input,
        )
        .expect_err("a malformed ORV1 argument must fail before the fixed socket");
        assert!(matches!(error, LocalRawCallError::Input));
        assert_eq!(
            error.to_string(),
            "orna: raw-call argument input is invalid"
        );
        assert!(error.source().is_none());
    }

    struct OneByteReader<'a> {
        bytes: &'a [u8],
        position: usize,
    }

    impl<'a> OneByteReader<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self { bytes, position: 0 }
        }
    }

    impl Read for OneByteReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.position >= self.bytes.len() {
                return Ok(0);
            }
            buffer[0] = self.bytes[self.position];
            self.position += 1;
            Ok(1)
        }
    }

    struct OversizedHeaderOnlyReader<'a> {
        header: &'a [u8],
        position: usize,
        payload_requested: bool,
    }

    impl<'a> OversizedHeaderOnlyReader<'a> {
        fn new(header: &'a [u8]) -> Self {
            Self {
                header,
                position: 0,
                payload_requested: false,
            }
        }
    }

    impl Read for OversizedHeaderOnlyReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.position >= self.header.len() {
                self.payload_requested = true;
                return Ok(0);
            }
            let available = self.header.len() - self.position;
            let length = available.min(buffer.len());
            buffer[..length].copy_from_slice(&self.header[self.position..self.position + length]);
            self.position += length;
            Ok(length)
        }
    }

    struct PairAggregateHeaderOnlyReader<'a> {
        bytes: &'a [u8],
        header_end: usize,
        position: usize,
        payload_requested: bool,
    }

    impl<'a> PairAggregateHeaderOnlyReader<'a> {
        fn new(bytes: &'a [u8], header_end: usize) -> Self {
            Self {
                bytes,
                header_end,
                position: 0,
                payload_requested: false,
            }
        }
    }

    impl Read for PairAggregateHeaderOnlyReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.position >= self.header_end {
                self.payload_requested = true;
                panic!("the aggregate check must reject before reading the second payload");
            }
            let available = self.header_end - self.position;
            let length = available.min(buffer.len());
            buffer[..length].copy_from_slice(&self.bytes[self.position..self.position + length]);
            self.position += length;
            Ok(length)
        }
    }

    fn require_raw_call_argument_input(error: LocalRawCallError) {
        assert!(matches!(error, LocalRawCallError::Input));
        assert_eq!(
            error.to_string(),
            "orna: raw-call argument input is invalid"
        );
        assert!(error.source().is_none());
    }

    #[test]
    fn raw_call_argument_input_rejects_every_invalid_boundary() {
        let _interrupt_guard = lock_interrupt_tests();
        INTERRUPTS.store(0, Ordering::SeqCst);
        let exact = [
            0x4f, 0x52, 0x56, 0x31, // ORV1 marker
            0x02, // Boolean tag
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Boolean type identity
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // Boolean type identity
            0x00, 0x00, 0x00, 0x01, // payload length
            0x01, // Boolean true payload
        ];
        let mut bad_marker = exact;
        bad_marker[..4].copy_from_slice(b"ORV2");
        let mut invalid_payload = exact;
        invalid_payload[VALUE_HEADER_LENGTH] = 0x02;
        for input in [
            Vec::new(),
            exact[..24].to_vec(),
            bad_marker.to_vec(),
            invalid_payload.to_vec(),
        ] {
            require_raw_call_argument_input(
                read_raw_call_argument(&mut Cursor::new(input))
                    .expect_err("invalid raw argument input must be rejected"),
            );
        }

        // A reader that returns at most one byte per read still decodes the
        // complete exact TRUE envelope through sequential partial reads.
        let mut one_byte = OneByteReader::new(&exact);
        assert_eq!(
            read_raw_call_argument(&mut one_byte).unwrap(),
            RuntimeValue::Boolean(true)
        );
        assert_eq!(one_byte.position, exact.len());

        // An oversized declared payload is rejected from the header alone,
        // before the production reader requests any payload byte.
        let mut oversized_header = exact[..VALUE_HEADER_LENGTH].to_vec();
        oversized_header[VALUE_HEADER_LENGTH - 4..]
            .copy_from_slice(&(MAX_ARGUMENT_VALUE_LENGTH as u32).to_be_bytes());
        let mut oversized = OversizedHeaderOnlyReader::new(&oversized_header);
        require_raw_call_argument_input(
            read_raw_call_argument(&mut oversized)
                .expect_err("an oversized declared payload must be rejected"),
        );
        assert_eq!(oversized.position, VALUE_HEADER_LENGTH);
        assert!(
            !oversized.payload_requested,
            "the bounded reader must reject the oversized envelope before payload bytes"
        );
    }

    struct InterruptOnFirstRead {
        reads: usize,
    }

    impl Read for InterruptOnFirstRead {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.reads > 1 {
                panic!("the interrupted input reader must not be read twice");
            }
            record_interrupt(0);
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "forced interrupt",
            ))
        }
    }

    #[test]
    fn raw_call_argument_input_interrupt_cancels_before_any_socket() {
        let _interrupt_guard = lock_interrupt_tests();
        INTERRUPTS.store(0, Ordering::SeqCst);
        // The ordinary unit-test environment has no fixed socket. If one is
        // unexpectedly present, skip this environmental precedence tracer.
        if require_fixed_socket().is_ok() {
            return;
        }
        assert!(
            matches!(require_fixed_socket(), Err(LocalRawCallError::Connection)),
            "the fixed socket boundary must be the connection failure in this environment"
        );
        let mut input = InterruptOnFirstRead { reads: 0 };
        let outcome = run_local_raw_call_with_argument_input(
            FunctionId::from_bytes([0x11; 16]),
            ParameterId::from_bytes([0x33; 16]),
            &mut input,
        )
        .expect("an input interrupt must cancel the call cleanly");
        assert_eq!(outcome, LocalRawCallOutcome::Cancelled);
        assert_eq!(input.reads, 1);
        assert_eq!(INTERRUPTS.load(Ordering::SeqCst), 1);
    }

    struct InterruptOnEofProbe<'a> {
        bytes: &'a [u8],
        position: usize,
        probes: usize,
    }

    impl<'a> InterruptOnEofProbe<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self {
                bytes,
                position: 0,
                probes: 0,
            }
        }
    }

    impl Read for InterruptOnEofProbe<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.position < self.bytes.len() {
                let available = self.bytes.len() - self.position;
                let length = available.min(buffer.len());
                buffer[..length]
                    .copy_from_slice(&self.bytes[self.position..self.position + length]);
                self.position += length;
                return Ok(length);
            }
            self.probes += 1;
            if self.probes > 1 {
                panic!("the EOF probe reader must not be read again");
            }
            record_interrupt(0);
            Ok(0)
        }
    }

    #[test]
    fn raw_call_argument_input_must_not_miss_an_interrupt_on_the_eof_probe() {
        let _interrupt_guard = lock_interrupt_tests();
        INTERRUPTS.store(0, Ordering::SeqCst);
        let exact = [
            0x4f, 0x52, 0x56, 0x31, // ORV1 marker
            0x02, // Boolean tag
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Boolean type identity
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // Boolean type identity
            0x00, 0x00, 0x00, 0x01, // payload length
            0x01, // Boolean true payload
        ];
        let mut input = InterruptOnEofProbe::new(&exact);
        let error = read_raw_call_argument(&mut input)
            .expect_err("an interrupt recorded by the EOF probe must surface as Signal");
        assert!(matches!(error, LocalRawCallError::Signal));
        assert_eq!(input.probes, 1);
        assert_eq!(INTERRUPTS.load(Ordering::SeqCst), 1);
    }

    struct InterruptAndTruncateOnFirstRead {
        reads: usize,
    }

    impl Read for InterruptAndTruncateOnFirstRead {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.reads > 1 {
                panic!("the truncated interrupt reader must not be read again");
            }
            record_interrupt(0);
            Ok(0)
        }
    }

    #[test]
    fn raw_call_argument_input_gives_signal_precedence_over_truncated_eof() {
        let _interrupt_guard = lock_interrupt_tests();
        INTERRUPTS.store(0, Ordering::SeqCst);
        let mut input = InterruptAndTruncateOnFirstRead { reads: 0 };
        let outcome = run_local_raw_call_with_argument_input(
            FunctionId::from_bytes([0x11; 16]),
            ParameterId::from_bytes([0x33; 16]),
            &mut input,
        )
        .expect("an interrupt with truncated EOF must cancel, not fail as input");
        assert_eq!(outcome, LocalRawCallOutcome::Cancelled);
        assert_eq!(input.reads, 1);
        assert_eq!(INTERRUPTS.load(Ordering::SeqCst), 1);
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
