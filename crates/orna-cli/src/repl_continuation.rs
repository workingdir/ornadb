//! Multi-line submissions for the REPL (spec `17-repl.md`, "Syntax-aware
//! editing": "multiline input until syntax is complete").
//!
//! A submission whose text parses as incomplete REPL input (an open bracket, a
//! trailing operator, an unfinished block) keeps reading lines under a `..`
//! continuation prompt. A blank line ends the submission as typed, so an input
//! that never completes reports its parse error at that point. Every other
//! line is appended, and the whole submission is bounded by the same input
//! limit as a single line. A submission that is complete or malformed is never
//! continued, and console commands (`:...`) are never continued.

use std::io::{self, BufRead, Write};

use crate::repl::{AnsiColor, MAX_INPUT_BYTES, ReadSubmission, write_styled};

const CONTINUATION_PROMPT: &[u8] = b".. ";

/// Reads one line onto `source`, returning the whole submission once a line
/// ends or the input ends. A submission over `MAX_INPUT_BYTES` is drained and
/// refused as `TooLong`.
pub(crate) fn read_submission<R: BufRead>(
    reader: &mut R,
    mut source: Vec<u8>,
) -> io::Result<ReadSubmission> {
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

/// Reads one REPL submission, continuing across lines while it is incomplete.
pub(crate) fn read_complete_submission<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<ReadSubmission> {
    let mut source = match read_submission(reader, Vec::new())? {
        ReadSubmission::Source(source) => source,
        other => return Ok(other),
    };
    if !continues(&source) {
        return Ok(ReadSubmission::Source(source));
    }
    loop {
        write_styled(writer, AnsiColor::Dim, CONTINUATION_PROMPT, color_enabled)?;
        writer.flush()?;
        let before = source.len();
        let mut next = match read_submission(reader, source.into_bytes())? {
            ReadSubmission::Source(next) => next,
            other => return Ok(other),
        };
        // No bytes arrived: the input ended, so the submission is what it is.
        if next.len() == before {
            return Ok(ReadSubmission::Source(next));
        }
        // A blank line ends the submission; the blank line itself is dropped.
        if next[before..].trim().is_empty() {
            next.truncate(before);
            return Ok(ReadSubmission::Source(next));
        }
        if !continues(&next) {
            return Ok(ReadSubmission::Source(next));
        }
        source = next;
    }
}

/// Whether `source` is a non-console submission that needs more lines. A
/// submission of blank lines and comments alone has no code to continue.
fn continues(source: &str) -> bool {
    let has_code = source.lines().any(|line| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with("//")
    });
    has_code
        && !source.trim_start().starts_with(':')
        && orna_syntax::parse_repl(source).is_incomplete()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    /// Reads one submission from `input`, returning it with the prompts written.
    fn read_one(input: &str) -> (ReadSubmission, String) {
        let mut reader = BufReader::new(Cursor::new(input.as_bytes().to_vec()));
        let mut output = Vec::new();
        let submission = read_complete_submission(&mut reader, &mut output, false)
            .expect("in-memory reads succeed");
        (
            submission,
            String::from_utf8(output).expect("UTF-8 prompts"),
        )
    }

    fn source(submission: ReadSubmission) -> String {
        match submission {
            ReadSubmission::Source(text) => text,
            _ => panic!("expected a source submission"),
        }
    }

    #[test]
    fn an_incomplete_expression_continues_until_it_completes() {
        let (submission, prompts) = read_one("let answer =\n  42;\n");
        assert_eq!(source(submission), "let answer =\n  42;\n");
        assert_eq!(prompts, ".. ");
    }

    #[test]
    fn an_open_bracket_continues_across_several_lines() {
        let (submission, prompts) = read_one("(1 +\n2 *\n3)\n");
        assert_eq!(source(submission), "(1 +\n2 *\n3)\n");
        assert_eq!(prompts, ".. .. ");
    }

    #[test]
    fn a_blank_line_ends_an_incomplete_submission_and_is_not_kept() {
        let mut reader = BufReader::new(Cursor::new(b"let answer =\n\n42\n".to_vec()));
        let mut output = Vec::new();
        let first = read_complete_submission(&mut reader, &mut output, false)
            .expect("in-memory reads succeed");
        assert_eq!(source(first), "let answer =\n");
        let second = read_complete_submission(&mut reader, &mut output, false)
            .expect("in-memory reads succeed");
        assert_eq!(source(second), "42\n");
        assert_eq!(String::from_utf8(output).expect("UTF-8"), ".. ");
    }

    #[test]
    fn a_comment_alone_is_not_continued() {
        let (submission, prompts) = read_one("// a note\n");
        assert_eq!(source(submission), "// a note\n");
        assert_eq!(prompts, "");
    }

    #[test]
    fn a_complete_expression_is_not_continued() {
        let (submission, prompts) = read_one("1 + 1\n");
        assert_eq!(source(submission), "1 + 1\n");
        assert_eq!(prompts, "");
    }

    #[test]
    fn a_malformed_expression_is_not_continued() {
        let (submission, prompts) = read_one(")\n");
        assert_eq!(source(submission), ")\n");
        assert_eq!(prompts, "");
    }

    #[test]
    fn a_blank_line_is_not_a_continuation() {
        let (submission, prompts) = read_one("   \n1\n");
        assert_eq!(source(submission), "   \n");
        assert_eq!(prompts, "");
    }

    #[test]
    fn a_console_command_is_not_continued() {
        let (submission, prompts) = read_one(":help (\n");
        assert_eq!(source(submission), ":help (\n");
        assert_eq!(prompts, "");
    }

    #[test]
    fn end_of_input_ends_an_incomplete_submission_without_looping() {
        let (submission, prompts) = read_one("let answer =");
        assert_eq!(source(submission), "let answer =");
        assert_eq!(prompts, ".. ");
    }

    #[test]
    fn end_of_input_after_a_newline_ends_an_incomplete_submission() {
        let (submission, prompts) = read_one("let answer =\n");
        assert_eq!(source(submission), "let answer =\n");
        assert_eq!(prompts, ".. ");
    }

    #[test]
    fn a_continued_submission_is_bounded_by_the_input_limit() {
        // Comment lines keep an open `let` incomplete without nesting, which the
        // parser caps (ORNA-PARSE-001), so the input limit is what ends it.
        let comment = format!("// {}\n", "x".repeat(60));
        let input = format!("let answer =\n{}", comment.repeat(2_000));
        let (submission, _) = read_one(&input);
        assert!(
            matches!(submission, ReadSubmission::TooLong),
            "a continued submission over the input limit is refused"
        );
    }
}
