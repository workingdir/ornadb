use std::{
    error::Error,
    fmt,
    io::{BufRead, Write},
};

use orna_core::InvocationId;
use orna_protocol::{
    InputRequested, SessionClientFrame, SessionCodecError, SessionInputState, SessionServerFrame,
    SessionStateError, decode_session_client_frame, decode_session_server_frame,
    encode_session_client_frame, encode_session_server_frame,
};
use orna_runtime_tty::{TerminalInput, TerminalInputError, TerminalInputReader};

/// Drives one bounded terminal session over `ORNA-SESSION/1` frames.
///
/// The driver owns prompt rendering and line input. It does not parse SQL or
/// Orna commands; the authenticated root evaluator remains responsible for
/// interpreting each returned line.
pub struct TerminalSessionDriver<R, W> {
    reader: TerminalInputReader<R>,
    output: W,
    state: SessionInputState,
    root_invocation_id: InvocationId,
    call_stream: u64,
}

impl<R, W> TerminalSessionDriver<R, W> {
    /// Creates a driver bound to one root invocation and call stream.
    pub fn new(
        reader: R,
        output: W,
        root_invocation_id: InvocationId,
        call_stream: u64,
    ) -> Result<Self, TerminalSessionDriverError> {
        let state = SessionInputState::new(root_invocation_id, call_stream)
            .map_err(TerminalSessionDriverError::State)?;
        Ok(Self {
            reader: TerminalInputReader::new(reader),
            output,
            state,
            root_invocation_id,
            call_stream,
        })
    }

    /// Responds to one decoded server input request.
    pub fn respond_to(
        &mut self,
        request: InputRequested,
    ) -> Result<SessionClientFrame, TerminalSessionDriverError>
    where
        R: BufRead,
        W: Write,
    {
        if request.root_invocation_id != self.root_invocation_id
            || request.call_stream != self.call_stream
        {
            return Err(TerminalSessionDriverError::MismatchedRequest);
        }
        self.state
            .request(request.request_invocation_id)
            .map_err(TerminalSessionDriverError::State)?;

        let frame = match self.reader.read_line(&mut self.output, &request.prompt) {
            Ok(TerminalInput::Line(line)) => SessionClientFrame::InputLine {
                root_invocation_id: self.root_invocation_id,
                call_stream: self.call_stream,
                request_invocation_id: request.request_invocation_id,
                line,
            },
            Ok(TerminalInput::Eof) => SessionClientFrame::InputEof {
                root_invocation_id: self.root_invocation_id,
                call_stream: self.call_stream,
                request_invocation_id: request.request_invocation_id,
            },
            Err(error) => SessionClientFrame::InputFailed {
                root_invocation_id: self.root_invocation_id,
                call_stream: self.call_stream,
                request_invocation_id: request.request_invocation_id,
                error: input_error_code(&error).to_owned(),
            },
        };
        self.state
            .accept(&frame)
            .map_err(TerminalSessionDriverError::State)?;
        Ok(frame)
    }

    /// Decodes one server input request and encodes its bounded response.
    pub fn respond_to_encoded(
        &mut self,
        encoded: &[u8],
    ) -> Result<Vec<u8>, TerminalSessionDriverError>
    where
        R: BufRead,
        W: Write,
    {
        let SessionServerFrame::InputRequested(request) =
            decode_session_server_frame(encoded).map_err(TerminalSessionDriverError::Protocol)?;
        let response = self.respond_to(request)?;
        encode_session_client_frame(&response).map_err(TerminalSessionDriverError::Protocol)
    }

    /// Returns whether the session received EOF or an input failure.
    pub const fn is_closed(&self) -> bool {
        self.state.is_closed()
    }

    /// Returns the pending server request identity, if one exists.
    pub const fn pending_request(&self) -> Option<InvocationId> {
        self.state.pending_request()
    }

    /// Returns the reader and prompt output after the session ends.
    pub fn into_parts(self) -> (R, W) {
        (self.reader.into_inner(), self.output)
    }
}

fn input_error_code(error: &TerminalInputError) -> &'static str {
    match error {
        TerminalInputError::Io(_) => "terminal.input_io",
        TerminalInputError::LineTooLong => "terminal.input_line_too_long",
        TerminalInputError::InvalidUtf8 => "terminal.input_invalid_utf8",
    }
}

/// A bounded TTY session driver failure.
#[derive(Debug)]
pub enum TerminalSessionDriverError {
    /// The session frame codec rejected an input request or response.
    Protocol(SessionCodecError),
    /// The session identity state rejected the request or response.
    State(SessionStateError),
    /// The request belongs to another root or stream.
    MismatchedRequest,
}

impl fmt::Display for TerminalSessionDriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => write!(formatter, "terminal session frame failed: {error}"),
            Self::State(error) => write!(formatter, "terminal session state failed: {error}"),
            Self::MismatchedRequest => {
                formatter.write_str("terminal session request identity is invalid")
            }
        }
    }
}

impl Error for TerminalSessionDriverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::State(error) => Some(error),
            Self::MismatchedRequest => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::*;

    fn id(value: u8) -> InvocationId {
        InvocationId::from_bytes([value; 16])
    }

    fn request(root: u8, stream: u64, request: u8, prompt: &str) -> InputRequested {
        InputRequested {
            root_invocation_id: id(root),
            call_stream: stream,
            request_invocation_id: id(request),
            prompt: prompt.to_owned(),
        }
    }

    fn driver(input: &[u8]) -> TerminalSessionDriver<BufReader<Cursor<Vec<u8>>>, Vec<u8>> {
        TerminalSessionDriver::new(
            BufReader::new(Cursor::new(input.to_vec())),
            Vec::new(),
            id(1),
            7,
        )
        .expect("driver creates")
    }

    #[test]
    fn responds_with_line_and_renders_prompt() {
        let mut driver = driver(b"select\n");
        let response = driver
            .respond_to(request(1, 7, 2, "orna> "))
            .expect("line response");
        assert_eq!(
            response,
            SessionClientFrame::InputLine {
                root_invocation_id: id(1),
                call_stream: 7,
                request_invocation_id: id(2),
                line: "select".to_owned(),
            }
        );
        let (_, output) = driver.into_parts();
        assert_eq!(output, b"orna> ");
    }

    #[test]
    fn responds_with_eof_and_closes_session() {
        let mut driver = driver(b"");
        let response = driver
            .respond_to(request(1, 7, 2, "orna> "))
            .expect("EOF response");
        assert!(matches!(response, SessionClientFrame::InputEof { .. }));
        assert!(driver.is_closed());
    }

    #[test]
    fn encodes_invalid_utf8_as_a_stable_failure() {
        let mut driver = driver(&[0xff, b'\n']);
        let request =
            encode_session_server_frame(&SessionServerFrame::InputRequested(request(1, 7, 2, "")))
                .expect("request encodes");
        let encoded = driver
            .respond_to_encoded(&request)
            .expect("failure response encodes");
        assert_eq!(
            decode_session_client_frame(&encoded).expect("failure decodes"),
            SessionClientFrame::InputFailed {
                root_invocation_id: id(1),
                call_stream: 7,
                request_invocation_id: id(2),
                error: "terminal.input_invalid_utf8".to_owned(),
            }
        );
        assert!(driver.is_closed());
    }

    #[test]
    fn rejects_foreign_request_without_writing_prompt() {
        let mut driver = driver(b"line\n");
        let error = driver
            .respond_to(request(9, 7, 2, "foreign> "))
            .expect_err("foreign request");
        assert!(matches!(
            error,
            TerminalSessionDriverError::MismatchedRequest
        ));
        let (_, output) = driver.into_parts();
        assert!(output.is_empty());
    }

    #[test]
    fn rejects_a_second_pending_request() {
        let mut driver = driver(b"line\nnext\n");
        driver.state.request(id(2)).expect("test pending request");
        let error = driver
            .respond_to(request(1, 7, 3, "orna> "))
            .expect_err("second request");
        assert!(matches!(
            error,
            TerminalSessionDriverError::State(SessionStateError::WrongState)
        ));
    }
}
