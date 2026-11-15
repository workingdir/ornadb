//! Located diagnostics for one REPL input, shared by every REPL evaluator so that a rejected input reads the same
//! on each path.

use std::ops::Range;

use crate::parser::Diagnostic;
use crate::{TokenKind, lex};

/// The first parse diagnostic of a rejected REPL input, located in the input and given its remedy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseFailure {
    /// The parser's own diagnostic code, for example `ORNA-PARSE-001`.
    pub code: &'static str,
    pub message: String,
    /// The byte span in the input, clamped to the written characters.
    pub span: Range<usize>,
    pub notes: Vec<String>,
    pub help: Vec<String>,
}

/// The first diagnostic of a failed parse of `source`, located and given its remedy. `None` when there is no
/// diagnostic to report.
#[must_use]
pub fn repl_parse_failure(diagnostics: &[Diagnostic], source: &str) -> Option<ParseFailure> {
    let diagnostic = diagnostics.first()?;
    // End-of-input tokens sit after the trailing newline the terminal
    // adds; point just past the last written character instead.
    let content_end = source.trim_end().len();
    let start = diagnostic.span.start.min(content_end);
    let end = diagnostic.span.end.min(content_end).max(start);
    let mut help = diagnostic.help.clone();
    if help.is_empty()
        && let Some(remedy) = parse_remedy(&diagnostic.message)
    {
        help.push(remedy.to_owned());
    }
    let mut notes = diagnostic.notes.clone();
    if diagnostic.message.starts_with("expected `)`")
        && let Some(opener) = unclosed_opener(source)
    {
        let (line, column) = line_column(source, opener.start);
        notes.push(format!(
            "the `(` at line {line}, column {column} is not closed"
        ));
    }
    Some(ParseFailure {
        code: diagnostic.code,
        message: diagnostic.message.clone(),
        span: start..end,
        notes,
        help,
    })
}

/// The remedy for a parser message the syntax crate leaves without `help`.
fn parse_remedy(message: &str) -> Option<&'static str> {
    if message == "expected namespace path" {
        Some("name the module to import after `use`, for example `use library;`")
    } else if message == "expected pattern" {
        Some("name the binding after `let`, for example `let x = 3;`")
    } else if message.starts_with("expected `)`") {
        Some("close the parenthesis with `)`")
    } else {
        None
    }
}

/// The innermost `(` that is still open at the end of `source`.
fn unclosed_opener(source: &str) -> Option<Range<usize>> {
    let mut open = Vec::new();
    for token in lex(source).ok()? {
        match token.kind {
            TokenKind::Punct("(") => open.push(token.span.start..token.span.end),
            TokenKind::Punct(")") => {
                open.pop();
            }
            _ => {}
        }
    }
    open.pop()
}

/// One-based line and column of byte `offset` in `source`.
fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    (line, before[line_start..].chars().count() + 1)
}

/// Locates the first identifier `name` in `source` as a byte span. With `qualified`, only an occurrence directly
/// after `.` (a member access) counts.
#[must_use]
pub fn locate_identifier(source: &str, name: &str, qualified: bool) -> Option<Range<usize>> {
    lex(source)
        .ok()?
        .into_iter()
        .find_map(|token| match &token.kind {
            TokenKind::Identifier { normalized } if normalized == name || token.text == name => {
                let start = token.span.start;
                (!qualified || source[..start].trim_end().ends_with('.'))
                    .then_some(start..token.span.end)
            }
            _ => None,
        })
}
