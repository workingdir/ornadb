//! Bounded control frames for client-owned session input.

use std::{error::Error, fmt};

use orna_core::InvocationId;

/// The fixed marker for session-control frames.
pub const SESSION_MARKER: &[u8; 14] = b"ORNA-SESSION/1";
/// The maximum UTF-8 byte length of one input line or prompt.
pub const MAX_SESSION_LINE_LENGTH: usize = 16 * 1024;
/// The maximum encoded session frame length.
pub const MAX_SESSION_FRAME_LENGTH: usize = 128 * 1024;
/// The maximum UTF-8 byte length of one session error.
pub const MAX_SESSION_ERROR_LENGTH: usize = 16 * 1024;

const SERVER_INPUT_REQUESTED: u8 = 0x01;
const CLIENT_INPUT_LINE: u8 = 0x81;
const CLIENT_INPUT_EOF: u8 = 0x82;
const CLIENT_INPUT_FAILED: u8 = 0x83;
const HEADER_LENGTH: usize = 59;

/// One server request for input from the client-owned session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputRequested {
    /// The root invocation that owns the session.
    pub root_invocation_id: InvocationId,
    /// The raw invocation call stream carrying the session.
    pub call_stream: u64,
    /// The request identity that must be echoed by the client.
    pub request_invocation_id: InvocationId,
    /// The bounded prompt shown by the client runtime.
    pub prompt: String,
}

/// One response to a server input request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionClientFrame {
    /// Supplies one bounded UTF-8 input line.
    InputLine {
        /// The root invocation that owns the session.
        root_invocation_id: InvocationId,
        /// The raw invocation call stream carrying the session.
        call_stream: u64,
        /// The request identity being answered.
        request_invocation_id: InvocationId,
        /// The input line without a line terminator.
        line: String,
    },
    /// Reports end of input for the session.
    InputEof {
        /// The root invocation that owns the session.
        root_invocation_id: InvocationId,
        /// The raw invocation call stream carrying the session.
        call_stream: u64,
        /// The request identity being answered.
        request_invocation_id: InvocationId,
    },
    /// Reports a bounded client-side input failure.
    InputFailed {
        /// The root invocation that owns the session.
        root_invocation_id: InvocationId,
        /// The raw invocation call stream carrying the session.
        call_stream: u64,
        /// The request identity being answered.
        request_invocation_id: InvocationId,
        /// The stable failure detail for operator diagnostics.
        error: String,
    },
}

/// A server-to-client session-control frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionServerFrame {
    /// Requests one line from the client-owned session.
    InputRequested(InputRequested),
}

/// A session frame codec failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionCodecError {
    /// The frame ended before its fixed header or payload completed.
    Truncated,
    /// The frame uses the opposite direction's tag.
    WrongDirection,
    /// The marker does not identify session protocol version one.
    InvalidMarker,
    /// The tag is not defined by this direction.
    InvalidTag,
    /// The payload length is inconsistent with the frame bytes.
    InvalidLength,
    /// A stream or invocation identity is zero.
    InvalidIdentity,
    /// A payload is not valid UTF-8.
    InvalidUtf8,
    /// A frame or payload exceeds its bound.
    Oversize,
    /// Bytes remain after the declared payload.
    TrailingData,
}

impl fmt::Display for SessionCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Truncated => "session frame is truncated",
            Self::WrongDirection => "session frame uses the wrong direction",
            Self::InvalidMarker => "session frame marker is invalid",
            Self::InvalidTag => "session frame tag is invalid",
            Self::InvalidLength => "session frame length is invalid",
            Self::InvalidIdentity => "session frame identity is invalid",
            Self::InvalidUtf8 => "session frame payload is not valid UTF-8",
            Self::Oversize => "session frame exceeds its bound",
            Self::TrailingData => "session frame has trailing data",
        })
    }
}

impl Error for SessionCodecError {}

/// A session state-machine failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStateError {
    /// The state already has a pending request or is closed.
    WrongState,
    /// The response does not match the current root, stream, or request.
    MismatchedIdentity,
    /// The request identity is zero.
    InvalidIdentity,
}

impl fmt::Display for SessionStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WrongState => "session input state is not ready",
            Self::MismatchedIdentity => "session input response identity is invalid",
            Self::InvalidIdentity => "session input request identity is invalid",
        })
    }
}

impl Error for SessionStateError {}

/// Tracks one authenticated session input request at a time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionInputState {
    root_invocation_id: InvocationId,
    call_stream: u64,
    pending_request: Option<InvocationId>,
    closed: bool,
}

impl SessionInputState {
    /// Creates state bound to one non-zero root invocation and call stream.
    pub fn new(
        root_invocation_id: InvocationId,
        call_stream: u64,
    ) -> Result<Self, SessionStateError> {
        if !valid_identity(root_invocation_id, call_stream, root_invocation_id) {
            return Err(SessionStateError::InvalidIdentity);
        }
        Ok(Self {
            root_invocation_id,
            call_stream,
            pending_request: None,
            closed: false,
        })
    }

    /// Records one request identity before sending its request frame.
    pub fn request(
        &mut self,
        request_invocation_id: InvocationId,
    ) -> Result<(), SessionStateError> {
        if self.closed || self.pending_request.is_some() {
            return Err(SessionStateError::WrongState);
        }
        if request_invocation_id.to_bytes() == [0; 16] {
            return Err(SessionStateError::InvalidIdentity);
        }
        self.pending_request = Some(request_invocation_id);
        Ok(())
    }

    /// Applies one client response and rejects late or crossed responses.
    pub fn accept(&mut self, frame: &SessionClientFrame) -> Result<(), SessionStateError> {
        if self.closed {
            return Err(SessionStateError::WrongState);
        }
        let (root, stream, request) = frame_identity(frame);
        if root != self.root_invocation_id
            || stream != self.call_stream
            || Some(request) != self.pending_request
        {
            return Err(SessionStateError::MismatchedIdentity);
        }
        self.pending_request = None;
        if matches!(
            frame,
            SessionClientFrame::InputEof { .. } | SessionClientFrame::InputFailed { .. }
        ) {
            self.closed = true;
        }
        Ok(())
    }

    /// Returns whether the session has received a terminal input response.
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Returns the currently pending request identity, if any.
    pub const fn pending_request(&self) -> Option<InvocationId> {
        self.pending_request
    }
}

fn valid_identity(root: InvocationId, stream: u64, request: InvocationId) -> bool {
    root.to_bytes() != [0; 16] && stream != 0 && request.to_bytes() != [0; 16]
}

fn frame_identity(frame: &SessionClientFrame) -> (InvocationId, u64, InvocationId) {
    match frame {
        SessionClientFrame::InputLine {
            root_invocation_id,
            call_stream,
            request_invocation_id,
            ..
        }
        | SessionClientFrame::InputEof {
            root_invocation_id,
            call_stream,
            request_invocation_id,
        }
        | SessionClientFrame::InputFailed {
            root_invocation_id,
            call_stream,
            request_invocation_id,
            ..
        } => (*root_invocation_id, *call_stream, *request_invocation_id),
    }
}

fn append_header(
    output: &mut Vec<u8>,
    tag: u8,
    root_invocation_id: InvocationId,
    call_stream: u64,
    request_invocation_id: InvocationId,
    payload_length: usize,
) {
    output.extend_from_slice(SESSION_MARKER);
    output.push(tag);
    output.extend_from_slice(&root_invocation_id.to_bytes());
    output.extend_from_slice(&call_stream.to_be_bytes());
    output.extend_from_slice(&request_invocation_id.to_bytes());
    output.extend_from_slice(&(payload_length as u32).to_be_bytes());
}

fn parse_header(
    encoded: &[u8],
    server_direction: bool,
) -> Result<(u8, InvocationId, u64, InvocationId, usize), SessionCodecError> {
    if encoded.len() > MAX_SESSION_FRAME_LENGTH {
        return Err(SessionCodecError::Oversize);
    }
    if encoded.len() < HEADER_LENGTH {
        return Err(SessionCodecError::Truncated);
    }
    if &encoded[..SESSION_MARKER.len()] != SESSION_MARKER {
        return Err(SessionCodecError::InvalidMarker);
    }
    let tag = encoded[SESSION_MARKER.len()];
    let server_tag = tag == SERVER_INPUT_REQUESTED;
    if server_tag != server_direction {
        return Err(SessionCodecError::WrongDirection);
    }
    if (!server_direction
        && !matches!(
            tag,
            CLIENT_INPUT_LINE | CLIENT_INPUT_EOF | CLIENT_INPUT_FAILED
        ))
        || (server_direction && !server_tag)
    {
        return Err(SessionCodecError::InvalidTag);
    }
    let mut root_bytes = [0_u8; 16];
    root_bytes.copy_from_slice(&encoded[15..31]);
    let root = InvocationId::from_bytes(root_bytes);
    let stream = u64::from_be_bytes(
        encoded[31..39]
            .try_into()
            .expect("session stream field has fixed length"),
    );
    let mut request_bytes = [0_u8; 16];
    request_bytes.copy_from_slice(&encoded[39..55]);
    let request = InvocationId::from_bytes(request_bytes);
    let payload_length = u32::from_be_bytes(
        encoded[55..59]
            .try_into()
            .expect("session payload length field has fixed length"),
    ) as usize;
    if !valid_identity(root, stream, request) {
        return Err(SessionCodecError::InvalidIdentity);
    }
    if payload_length > MAX_SESSION_FRAME_LENGTH - HEADER_LENGTH {
        return Err(SessionCodecError::Oversize);
    }
    if HEADER_LENGTH + payload_length > encoded.len() {
        return Err(SessionCodecError::Truncated);
    }
    if HEADER_LENGTH + payload_length < encoded.len() {
        return Err(SessionCodecError::TrailingData);
    }
    Ok((tag, root, stream, request, payload_length))
}

/// Encodes a server-to-client input request.
pub fn encode_session_server_frame(
    frame: &SessionServerFrame,
) -> Result<Vec<u8>, SessionCodecError> {
    let SessionServerFrame::InputRequested(request) = frame;
    if !valid_identity(
        request.root_invocation_id,
        request.call_stream,
        request.request_invocation_id,
    ) {
        return Err(SessionCodecError::InvalidIdentity);
    }
    let payload = request.prompt.as_bytes();
    if payload.len() > MAX_SESSION_LINE_LENGTH {
        return Err(SessionCodecError::Oversize);
    }
    let mut encoded = Vec::with_capacity(HEADER_LENGTH + payload.len());
    append_header(
        &mut encoded,
        SERVER_INPUT_REQUESTED,
        request.root_invocation_id,
        request.call_stream,
        request.request_invocation_id,
        payload.len(),
    );
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

/// Decodes a server-to-client input request.
pub fn decode_session_server_frame(
    encoded: &[u8],
) -> Result<SessionServerFrame, SessionCodecError> {
    let (_, root, stream, request, payload_length) = parse_header(encoded, true)?;
    if payload_length > MAX_SESSION_LINE_LENGTH {
        return Err(SessionCodecError::Oversize);
    }
    let prompt = std::str::from_utf8(&encoded[HEADER_LENGTH..])
        .map_err(|_| SessionCodecError::InvalidUtf8)?
        .to_owned();
    Ok(SessionServerFrame::InputRequested(InputRequested {
        root_invocation_id: root,
        call_stream: stream,
        request_invocation_id: request,
        prompt,
    }))
}

/// Encodes a client-to-server input response.
pub fn encode_session_client_frame(
    frame: &SessionClientFrame,
) -> Result<Vec<u8>, SessionCodecError> {
    let (tag, root, stream, request, payload, limit) = match frame {
        SessionClientFrame::InputLine {
            root_invocation_id,
            call_stream,
            request_invocation_id,
            line,
        } => (
            CLIENT_INPUT_LINE,
            *root_invocation_id,
            *call_stream,
            *request_invocation_id,
            line.as_bytes(),
            MAX_SESSION_LINE_LENGTH,
        ),
        SessionClientFrame::InputEof {
            root_invocation_id,
            call_stream,
            request_invocation_id,
        } => (
            CLIENT_INPUT_EOF,
            *root_invocation_id,
            *call_stream,
            *request_invocation_id,
            &[][..],
            0,
        ),
        SessionClientFrame::InputFailed {
            root_invocation_id,
            call_stream,
            request_invocation_id,
            error,
        } => (
            CLIENT_INPUT_FAILED,
            *root_invocation_id,
            *call_stream,
            *request_invocation_id,
            error.as_bytes(),
            MAX_SESSION_ERROR_LENGTH,
        ),
    };
    if !valid_identity(root, stream, request) {
        return Err(SessionCodecError::InvalidIdentity);
    }
    if payload.len() > limit {
        return Err(SessionCodecError::Oversize);
    }
    let mut encoded = Vec::with_capacity(HEADER_LENGTH + payload.len());
    append_header(&mut encoded, tag, root, stream, request, payload.len());
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

/// Decodes a client-to-server input response.
pub fn decode_session_client_frame(
    encoded: &[u8],
) -> Result<SessionClientFrame, SessionCodecError> {
    let (tag, root, stream, request, payload_length) = parse_header(encoded, false)?;
    let payload = &encoded[HEADER_LENGTH..];
    match tag {
        CLIENT_INPUT_LINE => {
            if payload_length > MAX_SESSION_LINE_LENGTH {
                return Err(SessionCodecError::Oversize);
            }
            Ok(SessionClientFrame::InputLine {
                root_invocation_id: root,
                call_stream: stream,
                request_invocation_id: request,
                line: std::str::from_utf8(payload)
                    .map_err(|_| SessionCodecError::InvalidUtf8)?
                    .to_owned(),
            })
        }
        CLIENT_INPUT_EOF => {
            if payload_length != 0 {
                return Err(SessionCodecError::InvalidLength);
            }
            Ok(SessionClientFrame::InputEof {
                root_invocation_id: root,
                call_stream: stream,
                request_invocation_id: request,
            })
        }
        CLIENT_INPUT_FAILED => {
            if payload_length > MAX_SESSION_ERROR_LENGTH {
                return Err(SessionCodecError::Oversize);
            }
            Ok(SessionClientFrame::InputFailed {
                root_invocation_id: root,
                call_stream: stream,
                request_invocation_id: request,
                error: std::str::from_utf8(payload)
                    .map_err(|_| SessionCodecError::InvalidUtf8)?
                    .to_owned(),
            })
        }
        _ => Err(SessionCodecError::InvalidTag),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> InvocationId {
        InvocationId::from_bytes([byte; 16])
    }

    fn request() -> InputRequested {
        InputRequested {
            root_invocation_id: id(1),
            call_stream: 3,
            request_invocation_id: id(2),
            prompt: "> ".to_owned(),
        }
    }

    fn line() -> SessionClientFrame {
        SessionClientFrame::InputLine {
            root_invocation_id: id(1),
            call_stream: 3,
            request_invocation_id: id(2),
            line: "select".to_owned(),
        }
    }

    #[test]
    fn server_request_round_trips() {
        let encoded = encode_session_server_frame(&SessionServerFrame::InputRequested(request()))
            .expect("request encodes");
        assert_eq!(
            decode_session_server_frame(&encoded),
            Ok(SessionServerFrame::InputRequested(request()))
        );
    }

    #[test]
    fn client_responses_round_trip() {
        for frame in [
            line(),
            SessionClientFrame::InputEof {
                root_invocation_id: id(1),
                call_stream: 3,
                request_invocation_id: id(2),
            },
            SessionClientFrame::InputFailed {
                root_invocation_id: id(1),
                call_stream: 3,
                request_invocation_id: id(2),
                error: "closed".to_owned(),
            },
        ] {
            let encoded = encode_session_client_frame(&frame).expect("response encodes");
            assert_eq!(decode_session_client_frame(&encoded), Ok(frame));
        }
    }

    #[test]
    fn codec_rejects_wrong_direction_and_trailing_bytes() {
        let server = encode_session_server_frame(&SessionServerFrame::InputRequested(request()))
            .expect("request encodes");
        assert_eq!(
            decode_session_client_frame(&server),
            Err(SessionCodecError::WrongDirection)
        );
        let mut trailing = server;
        trailing.push(0);
        assert_eq!(
            decode_session_server_frame(&trailing),
            Err(SessionCodecError::TrailingData)
        );
    }

    #[test]
    fn codec_rejects_invalid_utf8_and_oversize() {
        let mut invalid =
            encode_session_server_frame(&SessionServerFrame::InputRequested(request()))
                .expect("request encodes");
        invalid[55..59].copy_from_slice(&2_u32.to_be_bytes());
        invalid[HEADER_LENGTH] = 0xff;
        assert_eq!(
            decode_session_server_frame(&invalid),
            Err(SessionCodecError::InvalidUtf8)
        );
        let oversized = SessionClientFrame::InputLine {
            root_invocation_id: id(1),
            call_stream: 3,
            request_invocation_id: id(2),
            line: "x".repeat(MAX_SESSION_LINE_LENGTH + 1),
        };
        assert_eq!(
            encode_session_client_frame(&oversized),
            Err(SessionCodecError::Oversize)
        );
    }

    #[test]
    fn codec_rejects_zero_identities() {
        let mut request = request();
        request.root_invocation_id = InvocationId::from_bytes([0; 16]);
        assert_eq!(
            encode_session_server_frame(&SessionServerFrame::InputRequested(request)),
            Err(SessionCodecError::InvalidIdentity)
        );
    }

    #[test]
    fn state_rejects_duplicate_and_late_responses() {
        let mut state = SessionInputState::new(id(1), 3).expect("state creates");
        state.request(id(2)).expect("request starts");
        assert_eq!(state.request(id(3)), Err(SessionStateError::WrongState));
        state.accept(&line()).expect("line completes request");
        assert_eq!(
            state.accept(&line()),
            Err(SessionStateError::MismatchedIdentity)
        );
        state.request(id(4)).expect("second request starts");
        let eof = SessionClientFrame::InputEof {
            root_invocation_id: id(1),
            call_stream: 3,
            request_invocation_id: id(4),
        };
        state.accept(&eof).expect("EOF completes state");
        assert!(state.is_closed());
        assert_eq!(state.request(id(5)), Err(SessionStateError::WrongState));
    }
}
