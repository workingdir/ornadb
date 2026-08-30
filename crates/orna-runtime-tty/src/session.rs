//! Bounded terminal input for function-backed client sessions.
//!
//! This module owns terminal line transport only. It does not parse SQL or Orna
//! commands and it does not select a function. The caller supplies the prompt,
//! consumes one line, and sends that input through the normal client function
//! path.

use std::io::{self, BufRead, Write};

/// The maximum UTF-8 input line accepted by the terminal session.
pub const MAX_INPUT_LINE_BYTES: usize = 16 * 1024;

/// One bounded terminal input result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalInput {
    /// One input line without its line ending.
    Line(String),
    /// The input stream reached end-of-file.
    Eof,
}

/// A failure while reading one terminal session line.
#[derive(Debug)]
pub enum TerminalInputError {
    /// The terminal prompt or input stream failed.
    Io(io::Error),
    /// The input line exceeded [`MAX_INPUT_LINE_BYTES`].
    LineTooLong,
    /// The input was not valid UTF-8.
    InvalidUtf8,
}

impl std::fmt::Display for TerminalInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "terminal input failed: {error}"),
            Self::LineTooLong => write!(
                formatter,
                "terminal input line exceeds the {}-byte limit",
                MAX_INPUT_LINE_BYTES,
            ),
            Self::InvalidUtf8 => formatter.write_str("terminal input is not valid UTF-8"),
        }
    }
}

impl std::error::Error for TerminalInputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::LineTooLong | Self::InvalidUtf8 => None,
        }
    }
}

/// A caller-pumped, bounded terminal input reader.
///
/// The reader consumes one complete line at a time. It drains an overlong line
/// before returning its error, so the next request starts at a known boundary.
pub struct TerminalInputReader<R> {
    reader: R,
}

impl<R> TerminalInputReader<R> {
    /// Creates a terminal input reader over a buffered byte source.
    pub const fn new(reader: R) -> Self {
        Self { reader }
    }

    /// Returns the wrapped reader after the session ends.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

impl<R: BufRead> TerminalInputReader<R> {
    /// Writes one prompt and reads one bounded UTF-8 line.
    pub fn read_line(
        &mut self,
        output: &mut impl Write,
        prompt: &str,
    ) -> Result<TerminalInput, TerminalInputError> {
        output
            .write_all(prompt.as_bytes())
            .map_err(TerminalInputError::Io)?;
        output.flush().map_err(TerminalInputError::Io)?;

        let bytes = self.read_line_bytes()?;
        let Some(mut bytes) = bytes else {
            return Ok(TerminalInput::Eof);
        };
        if bytes.ends_with(b"\n") {
            bytes.pop();
            if bytes.ends_with(b"\r") {
                bytes.pop();
            }
        }
        String::from_utf8(bytes)
            .map(TerminalInput::Line)
            .map_err(|_| TerminalInputError::InvalidUtf8)
    }

    fn read_line_bytes(&mut self) -> Result<Option<Vec<u8>>, TerminalInputError> {
        let mut line = Vec::with_capacity(MAX_INPUT_LINE_BYTES.min(256));
        loop {
            let buffer = self.reader.fill_buf().map_err(TerminalInputError::Io)?;
            if buffer.is_empty() {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(line))
                };
            }

            let take = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1);
            let has_newline = buffer[take - 1] == b'\n';
            let next_length = line.len().saturating_add(take);
            if next_length > MAX_INPUT_LINE_BYTES {
                self.reader.consume(take);
                if !has_newline {
                    self.drain_until_newline()?;
                }
                return Err(TerminalInputError::LineTooLong);
            }
            line.extend_from_slice(&buffer[..take]);
            self.reader.consume(take);
            if has_newline {
                return Ok(Some(line));
            }
        }
    }

    fn drain_until_newline(&mut self) -> Result<(), TerminalInputError> {
        loop {
            let buffer = self.reader.fill_buf().map_err(TerminalInputError::Io)?;
            if buffer.is_empty() {
                return Ok(());
            }
            let take = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1);
            let has_newline = buffer[take - 1] == b'\n';
            self.reader.consume(take);
            if has_newline {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::*;

    #[test]
    fn writes_prompt_and_returns_line_without_ending() {
        let input = BufReader::new(Cursor::new(b"select\r\nnext\n".to_vec()));
        let mut reader = TerminalInputReader::new(input);
        let mut output = Vec::new();

        assert_eq!(
            reader.read_line(&mut output, "orna> ").expect("line"),
            TerminalInput::Line("select".to_owned()),
        );
        assert_eq!(output, b"orna> ");
    }

    #[test]
    fn reports_eof_after_consuming_the_last_partial_line() {
        let input = BufReader::new(Cursor::new(b"last".to_vec()));
        let mut reader = TerminalInputReader::new(input);
        let mut output = Vec::new();

        assert_eq!(
            reader.read_line(&mut output, "").expect("partial line"),
            TerminalInput::Line("last".to_owned()),
        );
        assert_eq!(
            reader.read_line(&mut output, "").expect("eof"),
            TerminalInput::Eof,
        );
    }

    #[test]
    fn drains_an_overlong_line_before_the_next_read() {
        let mut bytes = vec![b'x'; MAX_INPUT_LINE_BYTES + 1];
        bytes.extend_from_slice(b"\nnext\n");
        let input = BufReader::new(Cursor::new(bytes));
        let mut reader = TerminalInputReader::new(input);
        let mut output = Vec::new();

        assert!(matches!(
            reader.read_line(&mut output, ""),
            Err(TerminalInputError::LineTooLong),
        ));
        assert_eq!(
            reader.read_line(&mut output, "").expect("next line"),
            TerminalInput::Line("next".to_owned()),
        );
    }

    #[test]
    fn rejects_invalid_utf8_after_line_framing() {
        let input = BufReader::new(Cursor::new(vec![0xff, b'\n']));
        let mut reader = TerminalInputReader::new(input);
        let mut output = Vec::new();

        assert!(matches!(
            reader.read_line(&mut output, ""),
            Err(TerminalInputError::InvalidUtf8),
        ));
    }
}
