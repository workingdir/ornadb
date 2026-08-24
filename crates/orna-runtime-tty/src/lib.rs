//! The first OrnaDB runtime: terminal documents and byte streams.
//!
//! `orna-runtime-tty` consumes the two transient opaque value types
//! introduced by ADR 0057 and renders them to a sink writer:
//!
//! - `std.terminal.Document` payloads, framed as
//!   `ORNA-TERMINAL-DOCUMENT/1 <len:u32 be> <utf-8 bytes>` and rendered as
//!   plain text;
//! - `std.io.ByteStream` payloads, framed as
//!   `ORNA-BYTE-STREAM/1 <media-type-len:u32 be> <media-type> <len:u32 be>
//!   <bytes>` and rendered as raw bytes.
//!
//! The crate is independent of the database: it receives payload bytes plus
//! a sink writer, never a kernel or session. It validates the frame itself
//! rather than relying on the value codec that constructed it, and reports
//! no interactive surface beyond the two sinks in this slice. The client
//! ABI that selects and drives a runtime is a later ADR; this slice is the
//! renderer library.

use std::fmt;
use std::io::Write;

use orna_core::value::MAX_OPAQUE_CODEC_PAYLOAD_LENGTH;

/// The tty runtime family name (spec `docs/15-runtime-architecture.md`).
///
/// The client names this family in its `sys.invoke` runtime offer (ADR
/// 0063) and parses it from the `--runtime <family>` override. The identity
/// lives here so the family name is not duplicated in the server.
pub const RUNTIME_NAME: &str = "tty";

/// The installed tty runtime version.
///
/// Mirrors the workspace crate version so the client's runtime offer always
/// names the exact binary it links.
pub const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The terminal document frame magic, including the separating space.
const DOCUMENT_MAGIC: &[u8] = b"ORNA-TERMINAL-DOCUMENT/1 ";
/// The byte-stream frame magic, including the separating space.
const BYTE_STREAM_MAGIC: &[u8] = b"ORNA-BYTE-STREAM/1 ";
/// The width of a `u32 be` length prefix.
const LENGTH_PREFIX_LEN: usize = 4;

/// The sink that consumes a presented opaque value.
///
/// The `orna` client dispatches on the opaque result type: a
/// `std.terminal.Document` renders through [`Sink::Document`] and a
/// `std.io.ByteStream` through [`Sink::ByteStream`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Sink {
    /// Consumes `std.terminal.Document` payloads.
    Document,
    /// Consumes `std.io.ByteStream` payloads.
    ByteStream,
}

impl Sink {
    /// Renders one payload for this sink.
    ///
    /// See [`render_document`] and [`render_byte_stream`] for the exact
    /// validation and output rules.
    pub fn render(self, payload: &[u8], writer: &mut impl Write) -> Result<(), RuntimeTtyError> {
        match self {
            Self::Document => render_document(payload, writer),
            Self::ByteStream => render_byte_stream(payload, writer),
        }
    }
}

/// Renders one `std.terminal.Document` payload to `writer`.
///
/// Validates the `ORNA-TERMINAL-DOCUMENT/1` framing: the exact magic, a
/// `u32 be` body length matching the remaining bytes exactly, a UTF-8 body,
/// and no control codes (`\n` line separators are the only control
/// characters the layout permits), and requires a non-empty body ending
/// in a final newline. The body is written verbatim.
/// On rejection nothing is written to `writer`.
pub fn render_document(payload: &[u8], writer: &mut impl Write) -> Result<(), RuntimeTtyError> {
    let body = decode_document(payload)?;
    writer.write_all(body).map_err(RuntimeTtyError::Io)?;
    Ok(())
}

/// Renders one `std.io.ByteStream` payload to `writer`.
///
/// Validates the `ORNA-BYTE-STREAM/1` framing: the exact magic, a non-empty
/// `u32 be` media type, and a `u32 be` body length matching the remaining
/// bytes exactly. The body bytes are written verbatim with no UTF-8 or
/// control-character checks. On rejection nothing is written to `writer`.
pub fn render_byte_stream(payload: &[u8], writer: &mut impl Write) -> Result<(), RuntimeTtyError> {
    let body = decode_byte_stream(payload)?;
    writer.write_all(body).map_err(RuntimeTtyError::Io)?;
    Ok(())
}

/// Errors from validating and rendering a runtime payload.
#[derive(Debug)]
pub enum RuntimeTtyError {
    /// The payload does not start with the frame magic.
    InvalidMagic,
    /// The payload declares a length inconsistent with its remaining bytes.
    InvalidFrameLength,
    /// A document body is not valid UTF-8.
    InvalidUtf8,
    /// A document body carries a character the plain-text layout forbids.
    ControlCharacter,
    /// A document body is empty or does not end with a final newline.
    InvalidDocumentBody,
    /// A byte-stream payload declares an empty media type.
    InvalidMediaType,
    /// Writing the rendered output to the sink failed.
    Io(std::io::Error),
}

impl fmt::Display for RuntimeTtyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => formatter.write_str("payload has the wrong magic prefix"),
            Self::InvalidFrameLength => {
                formatter.write_str("payload has an inconsistent frame length")
            }
            Self::InvalidUtf8 => formatter.write_str("document payload is not valid UTF-8"),
            Self::ControlCharacter => {
                formatter.write_str("document payload contains a control character")
            }
            Self::InvalidDocumentBody => {
                formatter.write_str("document payload body is empty or lacks a final newline")
            }
            Self::InvalidMediaType => {
                formatter.write_str("byte-stream payload has an empty media type")
            }
            Self::Io(error) => write!(formatter, "failed to write rendered output: {error}"),
        }
    }
}

impl std::error::Error for RuntimeTtyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Validates one document payload and returns its body.
fn decode_document(payload: &[u8]) -> Result<&[u8], RuntimeTtyError> {
    let prefix_len = DOCUMENT_MAGIC
        .len()
        .checked_add(LENGTH_PREFIX_LEN)
        .ok_or(RuntimeTtyError::InvalidFrameLength)?;
    if payload.len() < prefix_len || !payload.starts_with(DOCUMENT_MAGIC) {
        return Err(if payload.starts_with(DOCUMENT_MAGIC) {
            RuntimeTtyError::InvalidFrameLength
        } else {
            RuntimeTtyError::InvalidMagic
        });
    }
    let body_len = u32::from_be_bytes(
        payload[DOCUMENT_MAGIC.len()..prefix_len]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    if body_len > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH {
        return Err(RuntimeTtyError::InvalidFrameLength);
    }
    let body = payload
        .get(prefix_len..)
        .ok_or(RuntimeTtyError::InvalidFrameLength)?;
    if body.len() != body_len {
        return Err(RuntimeTtyError::InvalidFrameLength);
    }
    let text = std::str::from_utf8(body).map_err(|_| RuntimeTtyError::InvalidUtf8)?;
    if text.chars().any(is_control) {
        return Err(RuntimeTtyError::ControlCharacter);
    }
    if body.is_empty() || !body.ends_with(b"\n") {
        return Err(RuntimeTtyError::InvalidDocumentBody);
    }
    Ok(body)
}

/// Validates one byte-stream payload and returns its body.
fn decode_byte_stream(payload: &[u8]) -> Result<&[u8], RuntimeTtyError> {
    let magic_end = BYTE_STREAM_MAGIC
        .len()
        .checked_add(LENGTH_PREFIX_LEN)
        .ok_or(RuntimeTtyError::InvalidFrameLength)?;
    if payload.len() < magic_end || !payload.starts_with(BYTE_STREAM_MAGIC) {
        return Err(if payload.starts_with(BYTE_STREAM_MAGIC) {
            RuntimeTtyError::InvalidFrameLength
        } else {
            RuntimeTtyError::InvalidMagic
        });
    }
    let media_type_len = u32::from_be_bytes(
        payload[BYTE_STREAM_MAGIC.len()..magic_end]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    if media_type_len == 0 {
        return Err(RuntimeTtyError::InvalidMediaType);
    }
    let media_type_end = magic_end
        .checked_add(media_type_len)
        .ok_or(RuntimeTtyError::InvalidFrameLength)?;
    let body_length_start = media_type_end
        .checked_add(LENGTH_PREFIX_LEN)
        .ok_or(RuntimeTtyError::InvalidFrameLength)?;
    let body = payload
        .get(body_length_start..)
        .ok_or(RuntimeTtyError::InvalidFrameLength)?;
    let body_len = u32::from_be_bytes(
        payload[media_type_end..body_length_start]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    if body_len > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH {
        return Err(RuntimeTtyError::InvalidFrameLength);
    }
    if body.len() != body_len {
        return Err(RuntimeTtyError::InvalidFrameLength);
    }
    Ok(body)
}

/// Returns whether `ch` is a control character the document layout forbids.
///
/// The layout permits `\n` line separators only; every other C0 control,
/// DEL, and the C1 controls are rejected.
fn is_control(ch: char) -> bool {
    ch != '\n' && matches!(ch, '\u{0000}'..='\u{001F}' | '\u{007F}'..='\u{009F}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_frame(body: &[u8]) -> Vec<u8> {
        let mut frame = DOCUMENT_MAGIC.to_vec();
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }

    fn byte_stream_frame(media_type: &[u8], body: &[u8]) -> Vec<u8> {
        let mut frame = BYTE_STREAM_MAGIC.to_vec();
        frame.extend_from_slice(&(media_type.len() as u32).to_be_bytes());
        frame.extend_from_slice(media_type);
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }

    fn render_document_to_vec(frame: &[u8]) -> Result<Vec<u8>, RuntimeTtyError> {
        let mut output = Vec::new();
        render_document(frame, &mut output)?;
        Ok(output)
    }

    fn render_byte_stream_to_vec(frame: &[u8]) -> Result<Vec<u8>, RuntimeTtyError> {
        let mut output = Vec::new();
        render_byte_stream(frame, &mut output)?;
        Ok(output)
    }

    /// A sink whose writes always fail.
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("sink is broken"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn assert_same_variant(actual: &RuntimeTtyError, expected: &RuntimeTtyError) {
        assert_eq!(
            std::mem::discriminant(actual),
            std::mem::discriminant(expected),
            "expected {expected:?}, got {actual:?}"
        );
    }

    fn assert_document_rejects(frame: &[u8], expected: RuntimeTtyError) {
        let mut output = Vec::new();
        let error = render_document(frame, &mut output).unwrap_err();
        assert_same_variant(&error, &expected);
        assert!(output.is_empty(), "rejected frames must not write output");
    }

    fn assert_byte_stream_rejects(frame: &[u8], expected: RuntimeTtyError) {
        let mut output = Vec::new();
        let error = render_byte_stream(frame, &mut output).unwrap_err();
        assert_same_variant(&error, &expected);
        assert!(output.is_empty(), "rejected frames must not write output");
    }

    #[test]
    fn renders_document_verbatim() {
        let output = render_document_to_vec(&document_frame(b"hello\nworld\n")).unwrap();
        assert_eq!(output, b"hello\nworld\n");
    }

    #[test]
    fn rejects_document_without_final_newline() {
        assert_document_rejects(
            &document_frame(b"hello\nworld"),
            RuntimeTtyError::InvalidDocumentBody,
        );
    }

    #[test]
    fn rejects_empty_document() {
        assert_document_rejects(&document_frame(b""), RuntimeTtyError::InvalidDocumentBody);
    }

    #[test]
    fn renders_unicode_document_verbatim() {
        let body = "naïve café \u{00e9} 🦀\n";
        let output = render_document_to_vec(&document_frame(body.as_bytes())).unwrap();
        assert_eq!(output, body.as_bytes());
    }

    #[test]
    fn rejects_document_with_wrong_magic() {
        let mut frame = document_frame(b"hello\n");
        frame[..4].copy_from_slice(b"WRNG");
        assert_document_rejects(&frame, RuntimeTtyError::InvalidMagic);
    }

    #[test]
    fn rejects_document_with_truncated_magic() {
        let frame = DOCUMENT_MAGIC[..5].to_vec();
        assert_document_rejects(&frame, RuntimeTtyError::InvalidMagic);
    }

    #[test]
    fn rejects_document_with_truncated_header() {
        let mut frame = DOCUMENT_MAGIC.to_vec();
        frame.extend_from_slice(&[0, 0]);
        assert_document_rejects(&frame, RuntimeTtyError::InvalidFrameLength);
    }

    #[test]
    fn rejects_document_when_declared_length_exceeds_body() {
        let mut frame = document_frame(b"hello\nworld\n");
        frame.truncate(frame.len() - 4);
        assert_document_rejects(&frame, RuntimeTtyError::InvalidFrameLength);
    }

    #[test]
    fn rejects_document_body_over_opaque_payload_limit() {
        let mut body = vec![b'a'; MAX_OPAQUE_CODEC_PAYLOAD_LENGTH];
        body.push(b'\n');
        assert_document_rejects(&document_frame(&body), RuntimeTtyError::InvalidFrameLength);
    }

    #[test]
    fn rejects_document_with_excess_trailing_bytes() {
        let mut frame = document_frame(b"hello\n");
        frame.extend_from_slice(b"extra");
        assert_document_rejects(&frame, RuntimeTtyError::InvalidFrameLength);
    }

    #[test]
    fn rejects_document_with_invalid_utf8() {
        assert_document_rejects(&document_frame(&[0xff, 0xfe]), RuntimeTtyError::InvalidUtf8);
    }

    #[test]
    fn rejects_document_control_characters() {
        for control in [b"\0", b"\t", b"\r", b"\x07", b"\x7f"] {
            assert_document_rejects(&document_frame(control), RuntimeTtyError::ControlCharacter);
        }
    }

    #[test]
    fn rejects_document_c1_control_character() {
        assert_document_rejects(
            &document_frame("\u{0085}".as_bytes()),
            RuntimeTtyError::ControlCharacter,
        );
    }

    #[test]
    fn renders_large_document() {
        let mut body = vec![b'a'; 1 << 20];
        body.push(b'\n');
        let output = render_document_to_vec(&document_frame(&body)).unwrap();
        assert_eq!(output, body);
    }

    #[test]
    fn reports_document_write_error() {
        let error = render_document(&document_frame(b"x\n"), &mut FailingWriter).unwrap_err();
        assert!(matches!(error, RuntimeTtyError::Io(_)));
    }

    #[test]
    fn renders_byte_stream_verbatim() {
        let body = b"{\"a\":1}\n";
        let output =
            render_byte_stream_to_vec(&byte_stream_frame(b"application/json", body)).unwrap();
        assert_eq!(output, body);
    }

    #[test]
    fn renders_binary_byte_stream_verbatim() {
        let body = [0x00, 0xff, 0x10, 0x80, 0x7f];
        let output =
            render_byte_stream_to_vec(&byte_stream_frame(b"application/octet-stream", &body))
                .unwrap();
        assert_eq!(output, body);
    }

    #[test]
    fn renders_empty_byte_stream_body() {
        let output =
            render_byte_stream_to_vec(&byte_stream_frame(b"application/octet-stream", b""))
                .unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn rejects_byte_stream_with_wrong_magic() {
        let mut frame = byte_stream_frame(b"text/plain", b"data");
        frame[..4].copy_from_slice(b"WRNG");
        assert_byte_stream_rejects(&frame, RuntimeTtyError::InvalidMagic);
    }

    #[test]
    fn rejects_byte_stream_with_truncated_header() {
        let mut frame = BYTE_STREAM_MAGIC.to_vec();
        frame.extend_from_slice(&[0, 0]);
        assert_byte_stream_rejects(&frame, RuntimeTtyError::InvalidFrameLength);
    }

    #[test]
    fn rejects_byte_stream_with_empty_media_type() {
        let frame = byte_stream_frame(b"", b"data");
        assert_byte_stream_rejects(&frame, RuntimeTtyError::InvalidMediaType);
    }

    #[test]
    fn rejects_byte_stream_with_truncated_media_type() {
        let mut frame = BYTE_STREAM_MAGIC.to_vec();
        frame.extend_from_slice(&10u32.to_be_bytes());
        frame.extend_from_slice(b"tex");
        assert_byte_stream_rejects(&frame, RuntimeTtyError::InvalidFrameLength);
    }

    #[test]
    fn rejects_byte_stream_when_declared_length_exceeds_body() {
        let mut frame = byte_stream_frame(b"text/plain", b"hello world");
        frame.truncate(frame.len() - 3);
        assert_byte_stream_rejects(&frame, RuntimeTtyError::InvalidFrameLength);
    }

    #[test]
    fn rejects_byte_stream_body_over_opaque_payload_limit() {
        let body = vec![0xa5; MAX_OPAQUE_CODEC_PAYLOAD_LENGTH + 1];
        assert_byte_stream_rejects(
            &byte_stream_frame(b"application/octet-stream", &body),
            RuntimeTtyError::InvalidFrameLength,
        );
    }

    #[test]
    fn rejects_byte_stream_with_excess_trailing_bytes() {
        let mut frame = byte_stream_frame(b"text/plain", b"data");
        frame.extend_from_slice(b"extra");
        assert_byte_stream_rejects(&frame, RuntimeTtyError::InvalidFrameLength);
    }

    #[test]
    fn rejects_byte_stream_with_huge_media_type_length() {
        let mut frame = BYTE_STREAM_MAGIC.to_vec();
        frame.extend_from_slice(&u32::MAX.to_be_bytes());
        frame.extend_from_slice(b"text/plain");
        assert_byte_stream_rejects(&frame, RuntimeTtyError::InvalidFrameLength);
    }

    #[test]
    fn renders_large_byte_stream() {
        let body = vec![0xabu8; 1 << 20];
        let output =
            render_byte_stream_to_vec(&byte_stream_frame(b"application/octet-stream", &body))
                .unwrap();
        assert_eq!(output, body);
    }

    #[test]
    fn reports_byte_stream_write_error() {
        let error = render_byte_stream(
            &byte_stream_frame(b"text/plain", b"data"),
            &mut FailingWriter,
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeTtyError::Io(_)));
    }

    #[test]
    fn sink_dispatches_document() {
        let mut output = Vec::new();
        Sink::Document
            .render(&document_frame(b"table\n"), &mut output)
            .unwrap();
        assert_eq!(output, b"table\n");
    }

    #[test]
    fn sink_dispatches_byte_stream() {
        let mut output = Vec::new();
        Sink::ByteStream
            .render(&byte_stream_frame(b"application/json", b"{}"), &mut output)
            .unwrap();
        assert_eq!(output, b"{}");
    }
}
