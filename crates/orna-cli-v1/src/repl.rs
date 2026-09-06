use std::fmt::Write as _;
use std::io::{self, BufRead, Write};

use orna_conformance_v1::AdmittedReplSession;
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

const MAX_INPUT_BYTES: usize = 65_536;
const MAX_INSPECT_DEPTH: usize = 4;
const MAX_INSPECT_ITEMS: usize = 16;
const MAX_INSPECT_TEXT: usize = 256;

enum ReadSubmission {
    Eof,
    TooLong,
    InvalidUtf8,
    Source(String),
}

/// Run one retained, line-oriented admitted REPL session. A malformed or
/// rejected submission reports its redacted evaluator code and leaves the
/// session open.
pub fn run<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    session: &mut AdmittedReplSession,
) -> io::Result<()> {
    loop {
        writer.write_all(b"> ")?;
        writer.flush()?;
        match read_submission(reader)? {
            ReadSubmission::Eof => return Ok(()),
            ReadSubmission::TooLong => writeln!(writer, "error[ORNA-REPL-INPUT-LIMIT]")?,
            ReadSubmission::InvalidUtf8 => writeln!(writer, "error[ORNA-REPL-INPUT-UTF8]")?,
            ReadSubmission::Source(source) if source.trim() == ":quit" => return Ok(()),
            ReadSubmission::Source(source) if source.trim_start().starts_with(':') => {
                writeln!(writer, "error[ORNA-REPL-COMMAND]")?;
            }
            ReadSubmission::Source(source) => match session.submit(&source) {
                Ok(Some(value)) => writeln!(writer, "{}", inspect(&value))?,
                Ok(None) => {}
                Err(error) => writeln!(writer, "error[{}]", error.code())?,
            },
        }
    }
}

fn read_submission<R: BufRead>(reader: &mut R) -> io::Result<ReadSubmission> {
    let mut source = Vec::new();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(if source.is_empty() {
                ReadSubmission::Eof
            } else {
                decode_submission(source)
            });
        }
        let take = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| position + 1);
        if source.len().saturating_add(take) > MAX_INPUT_BYTES {
            let complete = bytes.get(take - 1) == Some(&b'\n');
            reader.consume(take);
            if !complete {
                drain_line(reader)?;
            }
            return Ok(ReadSubmission::TooLong);
        }
        let complete = bytes.get(take - 1) == Some(&b'\n');
        source.extend_from_slice(&bytes[..take]);
        reader.consume(take);
        if complete {
            return Ok(decode_submission(source));
        }
    }
}

fn decode_submission(source: Vec<u8>) -> ReadSubmission {
    String::from_utf8(source).map_or(ReadSubmission::InvalidUtf8, ReadSubmission::Source)
}

fn drain_line<R: BufRead>(reader: &mut R) -> io::Result<()> {
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(());
        }
        let take = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| position + 1);
        let complete = bytes.get(take - 1) == Some(&b'\n');
        reader.consume(take);
        if complete {
            return Ok(());
        }
    }
}

pub fn inspect(value: &CanonicalValue) -> String {
    let (text, ty) = inspect_raw(value.raw(), 0);
    format!("{text} : {ty}")
}

fn inspect_raw(raw: &Raw, depth: usize) -> (String, &'static str) {
    if depth >= MAX_INSPECT_DEPTH {
        return ("…".into(), "Value");
    }
    match raw {
        Raw::Null => ("null".into(), "Null"),
        Raw::Bool(value) => (value.to_string(), "Bool"),
        Raw::Int(value) => (truncate(&value.to_string()), "Int"),
        Raw::Float(bits) => (format_float(*bits), "Float"),
        Raw::Bytes(bytes) => (inspect_bytes(bytes), "Bytes"),
        Raw::Text(value) => (format!("\"{}\"", escape(value)), "Str"),
        Raw::Array(values) => (inspect_sequence(values, depth), "Array"),
        Raw::Map(entries) => (inspect_map(entries, depth), "Map"),
        Raw::Tag(0, _) => ("<redacted>".into(), "Secret"),
        Raw::Tag(tag, value) => {
            let (text, _) = inspect_raw(value, depth + 1);
            (format!("Tag<{tag}>({text})"), "Tagged")
        }
    }
}

fn inspect_sequence(values: &[Raw], depth: usize) -> String {
    let mut items = values
        .iter()
        .take(MAX_INSPECT_ITEMS)
        .map(|value| inspect_raw(value, depth + 1).0)
        .collect::<Vec<_>>();
    if values.len() > MAX_INSPECT_ITEMS {
        items.push("…".into());
    }
    format!("[{}]", items.join(", "))
}

fn inspect_map(entries: &[(Raw, Raw)], depth: usize) -> String {
    let mut items = entries
        .iter()
        .take(MAX_INSPECT_ITEMS)
        .map(|(key, value)| {
            let key = inspect_raw(key, depth + 1).0;
            let value = inspect_raw(value, depth + 1).0;
            format!("{key}: {value}")
        })
        .collect::<Vec<_>>();
    if entries.len() > MAX_INSPECT_ITEMS {
        items.push("…".into());
    }
    format!("{{{}}}", items.join(", "))
}

fn inspect_bytes(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in bytes.iter().take(MAX_INSPECT_TEXT / 2) {
        write!(&mut text, "{byte:02x}").expect("writing to String is infallible");
    }
    if bytes.len() > MAX_INSPECT_TEXT / 2 {
        text.push('…');
    }
    format!("0x{text}")
}

fn format_float(bits: u64) -> String {
    let value = f64::from_bits(bits);
    if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value.is_sign_negative() {
            "-Infinity".into()
        } else {
            "Infinity".into()
        }
    } else {
        value.to_string()
    }
}

fn escape(value: &str) -> String {
    truncate(&value.escape_default().to_string())
}

fn truncate(value: &str) -> String {
    let mut end = value.len().min(MAX_INSPECT_TEXT);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut text = value[..end].to_owned();
    if end < value.len() {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_evaluator_v1::Limits;
    use std::io::BufReader;

    #[test]
    fn inspect_is_bounded_and_redacts_protected_values() {
        let value = CanonicalValue::protected();
        assert_eq!(inspect(&value), "<redacted> : Secret");
        assert_eq!(
            truncate(&"x".repeat(MAX_INSPECT_TEXT + 1)),
            format!("{}…", "x".repeat(MAX_INSPECT_TEXT))
        );
    }

    #[test]
    fn scripted_loop_admits_typed_declarations() {
        let mut input = b"let answer: Int = 42;\nanswer\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> > 42 : Int\n> "
        );
    }

    #[test]
    fn bounded_input_is_discarded_and_the_next_submission_runs() {
        let mut input = vec![b'x'; MAX_INPUT_BYTES + 1];
        input.extend_from_slice(b"\n1\n:quit\n");
        let mut input = input.as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL recovers");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> error[ORNA-REPL-INPUT-LIMIT]\n> 1 : Int\n> "
        );
    }

    #[test]
    fn unicode_input_may_span_bufread_chunks() {
        let source = "\"é\"\n:quit\n";
        let mut input = BufReader::with_capacity(1, source.as_bytes());
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> \"\\u{e9}\" : Str\n> "
        );
    }

    #[test]
    fn failed_submission_keeps_the_last_successful_result() {
        let mut input = b"1 + 1\nlet mismatch: Int = \"wrong\";\n$_\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.starts_with("> 2 : Int\n> error[ORNA-S021-TYPE]"));
        assert!(output.ends_with("> 2 : Int\n> "));
    }

    #[test]
    fn submitted_effect_is_rejected_without_changing_the_last_result() {
        let mut input =
            b"let seed: Int = 2;\nseed\nstd.net.http.get(\"https://example.com\")\n$_\n:quit\n"
                .as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> > 2 : Int\n> error[ORNA-REPL-EFFECT]\n> 2 : Int\n> "
        );
    }

    #[test]
    fn malformed_utf8_is_a_recoverable_submission_error() {
        let mut input = b"2\n\xff\n$_\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL recovers");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> 2 : Int\n> error[ORNA-REPL-INPUT-UTF8]\n> 2 : Int\n> "
        );
    }

    struct BrokenWriter;

    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("writer failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writer_errors_are_propagated() {
        let mut input = b":quit\n".as_slice();
        let mut writer = BrokenWriter;
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            run(&mut input, &mut writer, &mut session)
                .expect_err("writer error")
                .kind(),
            io::ErrorKind::Other
        );
    }
}
