//! `:copy EXPR [--format orna|json|csv]`: writes one expression's value as
//! canonical text that can be copied out of the console.
//!
//! The text comes from the standard codec itself. The console previews
//! `std.encoding.orna.encode(EXPR)` or `std.encoding.json.encode(EXPR)` through
//! the same path `:inspect` uses, so an unsupported value fails with the
//! codec's own diagnostic and no console-side encoder can drift from the
//! language. `csv` is refused: CSV is an optional codec package that this
//! runtime does not provide (spec `09-standard-library.md`, codec section).

use std::io::{self, Write};
use std::ops::Range;

use orna_evaluator::{RelationDisplay, ReplValue};
use orna_project_repl::{AdmittedReplSession, ReplDetail};
use orna_value::Raw;

use crate::repl::{ReplFailure, write_repl_error, write_repl_failure};

/// The code every `:copy` refusal carries, shared with the other console
/// commands.
const COPY_CODE: &str = "ORNA-REPL-COMMAND";

const USAGE: &str = "usage: :copy EXPR [--format orna|json]";

/// A refused `:copy` argument: the message and the offending span, if any.
type ParseError = (String, Option<Range<usize>>);

/// A text codec `:copy` can produce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CopyFormat {
    Orna,
    Json,
}

impl CopyFormat {
    /// The standard codec call that encodes a value in this format.
    fn encoder(self) -> &'static str {
        match self {
            Self::Orna => "std.encoding.orna.encode",
            Self::Json => "std.encoding.json.encode",
        }
    }
}

/// Parses the argument of `:copy`: the expression, and an optional trailing
/// `--format NAME` that defaults to `orna`.
///
/// On failure, returns the message and the span of the offending text within
/// `arg`, when there is one.
pub(crate) fn parse(arg: &str) -> Result<(&str, CopyFormat), ParseError> {
    let (expr, format) = match arg.rfind("--format") {
        Some(index) if index == 0 || arg[..index].ends_with(char::is_whitespace) => {
            let expr = arg[..index].trim_end();
            let name_start = index + "--format".len();
            let name = arg[name_start..].trim();
            let format = match name {
                "orna" => CopyFormat::Orna,
                "json" => CopyFormat::Json,
                "csv" => {
                    return Err((
                        "`:copy --format csv` is not available: CSV is an optional codec \
                         package this runtime does not provide"
                            .into(),
                        None,
                    ));
                }
                _ => {
                    return Err((
                        format!("unknown copy format `{name}`; expected orna or json"),
                        None,
                    ));
                }
            };
            (expr, format)
        }
        _ => (arg.trim_end(), CopyFormat::Orna),
    };
    if expr.trim().is_empty() {
        return Err((USAGE.into(), None));
    }
    Ok((expr.trim_start(), format))
}

/// Runs `:copy`: writes the codec text of the expression, or the located
/// refusal when it cannot be produced.
pub(crate) fn run<W: Write>(
    arg: &str,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    let (expr, format) = match parse(arg) {
        Ok(parsed) => parsed,
        Err((message, span)) => {
            let failure = refusal(message, span, Vec::new());
            return write_repl_failure(writer, &failure, arg, color_enabled);
        }
    };
    let prefix = format.encoder().len() + "(".len();
    let wrapped = format!("{}({expr})", format.encoder());
    let outcome = match session.preview_outcome(&wrapped, RelationDisplay::Window) {
        Ok(outcome) => outcome,
        Err(error) => {
            let failure = relocate(ReplFailure::from(error), prefix, expr.len());
            return write_repl_failure(writer, &failure, expr, color_enabled);
        }
    };
    let text = match outcome.value {
        Some(ReplValue::Value(value)) => match value.raw() {
            Raw::Text(text) => text.clone(),
            _ => return write_repl_error(writer, "ORNA-EVAL-VALUE", color_enabled),
        },
        _ => return write_repl_error(writer, "ORNA-EVAL-VALUE", color_enabled),
    };
    writer.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        writer.write_all(b"\n")?;
    }
    Ok(())
}

/// Maps a span in the wrapped call back onto the expression the user typed.
/// A span that falls in the codec call's own text has no user-visible
/// position and is dropped.
fn relocate(mut failure: ReplFailure, prefix: usize, expr_len: usize) -> ReplFailure {
    if let Some(detail) = failure.detail.as_mut() {
        detail.span = detail.span.take().and_then(|span| {
            (span.start >= prefix && span.end <= prefix + expr_len)
                .then(|| span.start - prefix..span.end - prefix)
        });
    }
    failure
}

fn refusal(message: String, span: Option<Range<usize>>, help: Vec<String>) -> ReplFailure {
    ReplFailure {
        code: COPY_CODE.into(),
        detail: Some(ReplDetail {
            code: COPY_CODE.into(),
            message,
            span,
            notes: Vec::new(),
            help,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{CopyFormat, parse};

    #[test]
    fn defaults_to_orna_and_keeps_expression_text() {
        assert_eq!(parse("1 + 2").unwrap(), ("1 + 2", CopyFormat::Orna));
        assert_eq!(
            parse("  [1, 2]  --format json").unwrap(),
            ("[1, 2]", CopyFormat::Json)
        );
        assert_eq!(
            parse("\"--format\" --format orna").unwrap(),
            ("\"--format\"", CopyFormat::Orna)
        );
    }

    #[test]
    fn refuses_csv_unknown_formats_and_missing_expressions() {
        assert!(parse("1 --format csv").unwrap_err().0.contains("csv"));
        assert!(
            parse("1 --format xml")
                .unwrap_err()
                .0
                .contains("unknown copy format `xml`")
        );
        assert_eq!(parse("").unwrap_err().0, super::USAGE);
        assert_eq!(parse("--format json").unwrap_err().0, super::USAGE);
    }
}
