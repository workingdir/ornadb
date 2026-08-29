use std::{
    error::Error,
    fmt,
    io::{self, Read, Write},
};

use orna_core::{revision::ActiveDatabaseRevision, value::OpaqueCodecRegistry};
use orna_protocol::{
    ClientFrame, FrameCodecError, InvocationClient, InvocationClientError,
    InvocationClientResponse, MAX_FRAME_PAYLOAD_LENGTH, RetainedInvokeRequest,
    encode_constructed_client_frame,
};

const FRAME_HEADER_LENGTH: usize = 18;
const CONSTRUCTED_PROTOCOL_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00";
const CONSTRUCTED_PROTOCOL_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00";

/// A framed constructed-protocol client over an already-authenticated stream.
///
/// The caller owns connection setup and transport authentication. This type
/// owns only frame I/O and one invocation lifecycle, so Unix, SSH-forwarded,
/// TCP, and TLS streams can share the same invocation path.
pub struct InvocationConnection<S> {
    stream: S,
    active: ActiveDatabaseRevision,
    registry: OpaqueCodecRegistry,
    client: InvocationClient,
}

impl<S: Read + Write> InvocationConnection<S> {
    /// Completes the constructed Orna protocol handshake on an authenticated stream.
    ///
    /// The caller remains responsible for local peer authentication or remote TLS.
    /// This method negotiates framing only and does not establish a principal.
    pub fn handshake(stream: &mut S) -> Result<(), InvocationConnectionError> {
        stream
            .write_all(&CONSTRUCTED_PROTOCOL_HELLO)
            .map_err(InvocationConnectionError::Io)?;
        stream.flush().map_err(InvocationConnectionError::Io)?;

        let mut acknowledgement = [0_u8; CONSTRUCTED_PROTOCOL_ACK.len()];
        stream
            .read_exact(&mut acknowledgement)
            .map_err(InvocationConnectionError::Io)?;
        if acknowledgement != CONSTRUCTED_PROTOCOL_ACK {
            return Err(InvocationConnectionError::HandshakeRejected);
        }
        Ok(())
    }

    /// Performs the constructed protocol handshake and starts one invocation.
    pub fn connect(
        mut stream: S,
        active: ActiveDatabaseRevision,
        registry: OpaqueCodecRegistry,
        request: RetainedInvokeRequest,
    ) -> Result<Self, InvocationConnectionError> {
        Self::handshake(&mut stream)?;
        Self::start(stream, active, registry, request)
    }
    /// Starts one invocation and writes its initial frames in protocol order.
    ///
    /// The stream must already have completed the Orna transport handshake.
    pub fn start(
        mut stream: S,
        active: ActiveDatabaseRevision,
        registry: OpaqueCodecRegistry,
        request: RetainedInvokeRequest,
    ) -> Result<Self, InvocationConnectionError> {
        let (client, frames) = InvocationClient::start(request);
        for frame in frames {
            write_client_frame(&mut stream, &active, &registry, &frame)?;
        }
        Ok(Self {
            stream,
            active,
            registry,
            client,
        })
    }

    /// Requests cancellation and writes the corresponding control frame.
    pub fn request_cancellation(&mut self) -> Result<(), InvocationConnectionError> {
        let frame = self
            .client
            .request_cancellation()
            .map_err(InvocationConnectionError::Client)?;
        write_client_frame(&mut self.stream, &self.active, &self.registry, &frame)
    }

    /// Reads and validates the next server frame.
    pub fn receive(&mut self) -> Result<InvocationClientResponse, InvocationConnectionError> {
        let encoded = read_server_frame(&mut self.stream)?;
        self.client
            .receive_encoded(&self.active, &self.registry, &encoded)
            .map_err(InvocationConnectionError::Client)
    }

    /// Writes one additional client control frame for this invocation.
    ///
    /// The invocation lifecycle helper remains authoritative for response
    /// frames. Window updates, liveness pings, and cancellation are accepted;
    /// a second invocation start or argument frame is rejected.
    pub fn write_control(&mut self, frame: &ClientFrame) -> Result<(), InvocationConnectionError> {
        match frame {
            ClientFrame::Ping { .. } => {}
            ClientFrame::WindowUpdate { stream, .. } if *stream == self.client.stream() => {}
            ClientFrame::CallCancel { stream } if *stream == self.client.stream() => {
                let expected = self
                    .client
                    .request_cancellation()
                    .map_err(InvocationConnectionError::Client)?;
                if &expected != frame {
                    return Err(InvocationConnectionError::InvalidControlFrame);
                }
            }
            _ => return Err(InvocationConnectionError::InvalidControlFrame),
        }
        write_client_frame(&mut self.stream, &self.active, &self.registry, frame)
    }

    /// Returns the invocation state owner.
    pub fn client(&self) -> &InvocationClient {
        &self.client
    }

    /// Returns mutable access to the invocation state owner.
    pub fn client_mut(&mut self) -> &mut InvocationClient {
        &mut self.client
    }

    /// Returns the underlying stream after the invocation ends.
    pub fn into_inner(self) -> S {
        self.stream
    }
}

/// A failure while writing or reading an invocation frame.
#[derive(Debug)]
#[non_exhaustive]
pub enum InvocationConnectionError {
    /// The underlying stream failed.
    Io(io::Error),
    /// The frame codec rejected a complete frame.
    Frame(FrameCodecError),
    /// The invocation lifecycle rejected a server frame.
    Client(InvocationClientError),
    /// A server did not accept the constructed protocol handshake.
    HandshakeRejected,
    /// The caller attempted to write a second invocation start frame.
    InvalidControlFrame,
}

impl fmt::Display for InvocationConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(formatter, "invocation transport I/O failed: {source}"),
            Self::Frame(source) => write!(formatter, "invocation transport frame failed: {source}"),
            Self::Client(source) => {
                write!(formatter, "invocation transport lifecycle failed: {source}")
            }
            Self::HandshakeRejected => {
                formatter.write_str("invocation transport handshake was rejected")
            }
            Self::InvalidControlFrame => {
                formatter.write_str("invocation transport control frame is not allowed")
            }
        }
    }
}

impl Error for InvocationConnectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Frame(source) => Some(source),
            Self::Client(source) => Some(source),
            Self::HandshakeRejected | Self::InvalidControlFrame => None,
        }
    }
}

fn write_client_frame<S: Write>(
    stream: &mut S,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    frame: &ClientFrame,
) -> Result<(), InvocationConnectionError> {
    let encoded = encode_constructed_client_frame(active, registry, frame)
        .map_err(InvocationConnectionError::Frame)?;
    stream
        .write_all(&encoded)
        .map_err(InvocationConnectionError::Io)?;
    stream.flush().map_err(InvocationConnectionError::Io)
}

fn read_server_frame<S: Read>(stream: &mut S) -> Result<Vec<u8>, InvocationConnectionError> {
    let mut header = [0_u8; FRAME_HEADER_LENGTH];
    stream
        .read_exact(&mut header)
        .map_err(InvocationConnectionError::Io)?;
    let payload_length = u32::from_be_bytes(
        header[14..18]
            .try_into()
            .expect("fixed frame header payload length has four bytes"),
    ) as usize;
    if payload_length > MAX_FRAME_PAYLOAD_LENGTH {
        return Err(InvocationConnectionError::Frame(
            FrameCodecError::PayloadTooLarge {
                actual: payload_length,
                maximum: MAX_FRAME_PAYLOAD_LENGTH,
            },
        ));
    }
    let mut encoded = Vec::with_capacity(FRAME_HEADER_LENGTH + payload_length);
    encoded.extend_from_slice(&header);
    encoded.resize(FRAME_HEADER_LENGTH + payload_length, 0);
    stream
        .read_exact(&mut encoded[FRAME_HEADER_LENGTH..])
        .map_err(InvocationConnectionError::Io)?;
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, Read, Write};

    use super::*;

    struct MemoryStream {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl MemoryStream {
        fn with_input(input: Vec<u8>) -> Self {
            Self {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }
    }

    impl Read for MemoryStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for MemoryStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn constructed_handshake_writes_hello_and_accepts_acknowledgement() {
        let mut stream = MemoryStream::with_input(CONSTRUCTED_PROTOCOL_ACK.to_vec());

        InvocationConnection::<MemoryStream>::handshake(&mut stream)
            .expect("constructed handshake should succeed");

        assert_eq!(stream.output, CONSTRUCTED_PROTOCOL_HELLO);
    }

    #[test]
    fn constructed_handshake_rejects_wrong_acknowledgement() {
        let mut acknowledgement = CONSTRUCTED_PROTOCOL_ACK;
        acknowledgement[0] = b'X';
        let mut stream = MemoryStream::with_input(acknowledgement.to_vec());

        let error = InvocationConnection::<MemoryStream>::handshake(&mut stream)
            .expect_err("wrong acknowledgement must fail");

        assert!(matches!(
            error,
            InvocationConnectionError::HandshakeRejected
        ));
        assert_eq!(stream.output, CONSTRUCTED_PROTOCOL_HELLO);
    }

    #[test]
    fn frame_reader_rejects_oversized_payload_before_allocation() {
        let mut header = [0_u8; FRAME_HEADER_LENGTH];
        header[14..18].copy_from_slice(
            &(u32::try_from(MAX_FRAME_PAYLOAD_LENGTH + 1).expect("test payload length fits u32"))
                .to_be_bytes(),
        );
        let error = read_server_frame(&mut Cursor::new(header))
            .expect_err("oversized payload must fail before reading its body");
        assert!(matches!(
            error,
            InvocationConnectionError::Frame(FrameCodecError::PayloadTooLarge {
                actual,
                maximum,
            }) if actual == MAX_FRAME_PAYLOAD_LENGTH + 1 && maximum == MAX_FRAME_PAYLOAD_LENGTH
        ));
    }
}
