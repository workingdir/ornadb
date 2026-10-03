//! Browser-safe Orna REPL core and stable JSON adapter.

use orna_evaluator_v1::{AdmittedReplSession, Limits, ReplError};
use orna_foundation_v1::{CanonicalValue, OvbRaw, canonical_uuid_text};
use orna_syntax_v1::parse_repl_with_file;
use serde::Serialize;
use wasm_bindgen::prelude::*;

const MAX_INSPECT_DEPTH: usize = 4;
const MAX_INSPECT_ITEMS: usize = 16;
const MAX_INSPECT_TEXT: usize = 256;

#[derive(Clone, Debug, Serialize)]
struct ReplResult {
    kind: &'static str,
    text: String,
}

#[derive(Clone, Debug, Serialize)]
struct RunError {
    message: String,
    line: u32,
    col: u32,
}

#[derive(Debug, Serialize)]
struct RunResult {
    ok: bool,
    values: Vec<String>,
    stdout: String,
    errors: Vec<RunError>,
}

/// Runs source in a fresh, deterministic in-memory REPL session.
///
/// Each complete REPL input is evaluated in order. Successful values are
/// rendered with the same bounded value/type presentation as the Orna CLI.
/// Declarations update session state and are represented as echoes by the
/// line-oriented API; they do not contribute to `values` or `stdout`.
#[wasm_bindgen]
pub fn run(source: &str) -> String {
    let mut session = ReplSession::new();
    let mut values = Vec::new();
    let mut errors = Vec::new();
    let mut pending = String::new();
    let mut pending_line = 1_u32;

    for (index, physical_line) in source.split_inclusive('\n').enumerate() {
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if pending.is_empty() {
            let trimmed = physical_line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }
            pending_line = line_number;
        }
        pending.push_str(physical_line);
        if !orna_syntax_v1::parse_repl(&pending).is_incomplete() {
            let (result, error) = session.evaluate_source(&pending, pending_line);
            if result.kind == "value" {
                values.push(result.text);
            }
            if let Some(error) = error {
                errors.push(error);
            }
            pending.clear();
        }
    }

    if !pending.is_empty() {
        let (result, error) = session.evaluate_source(&pending, pending_line);
        if result.kind == "value" {
            values.push(result.text);
        }
        if let Some(error) = error {
            errors.push(error);
        }
    }

    let stdout = if values.is_empty() {
        String::new()
    } else {
        format!("{}\n", values.join("\n"))
    };
    encode_json(&RunResult {
        ok: errors.is_empty(),
        values,
        stdout,
        errors,
    })
}

/// Stateful browser REPL. Bindings and the last result remain in this
/// in-memory session until it is dropped.
#[wasm_bindgen]
pub struct ReplSession {
    session: AdmittedReplSession,
}

#[wasm_bindgen]
impl ReplSession {
    /// Creates an empty session with the standard Orna resource limits.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            session: AdmittedReplSession::new(Limits::default()),
        }
    }

    /// Evaluates one expression or declaration and returns stable JSON with
    /// `kind` set to `value`, `error`, or `echo`.
    pub fn evaluate(&mut self, line: &str) -> String {
        let (result, _) = self.evaluate_source(line, 1);
        encode_json(&result)
    }
}

impl ReplSession {
    fn evaluate_source(&mut self, source: &str, base_line: u32) -> (ReplResult, Option<RunError>) {
        if source.trim().is_empty() || source.trim_start().starts_with("//") {
            return (
                ReplResult {
                    kind: "echo",
                    text: String::new(),
                },
                None,
            );
        }

        match self.session.submit(source) {
            Ok(Some(value)) => (
                ReplResult {
                    kind: "value",
                    text: inspect(&value),
                },
                None,
            ),
            Ok(None) => (
                ReplResult {
                    kind: "echo",
                    text: source.trim().to_owned(),
                },
                None,
            ),
            Err(error) => {
                let diagnostic = parse_repl_with_file(source, "<playground>")
                    .diagnostics
                    .into_iter()
                    .next();
                let (message, line, col) = diagnostic.map_or_else(
                    || {
                        (
                            error.code().to_owned(),
                            base_line,
                            first_non_whitespace_column(source),
                        )
                    },
                    |diagnostic| {
                        let position = diagnostic.span.start_position;
                        let line = position.as_ref().map_or(1, |position| position.line);
                        let col = position.as_ref().map_or(1, |position| position.column);
                        (
                            format!("{}: {}", diagnostic.code, diagnostic.message),
                            base_line.saturating_add(line.saturating_sub(1)),
                            col,
                        )
                    },
                );
                let run_error = RunError {
                    message: message.clone(),
                    line,
                    col,
                };
                (
                    ReplResult {
                        kind: "error",
                        text: message,
                    },
                    Some(run_error),
                )
            }
        }
    }
}

fn first_non_whitespace_column(source: &str) -> u32 {
    let column = source
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take_while(|scalar| scalar.is_whitespace())
        .count()
        .saturating_add(1);
    u32::try_from(column).unwrap_or(u32::MAX)
}

fn encode_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned())
}

fn inspect(value: &CanonicalValue) -> String {
    let (text, ty) = inspect_raw(value.raw(), 0);
    format!("{text} : {ty}")
}

fn inspect_raw(raw: &OvbRaw, depth: usize) -> (String, &'static str) {
    if depth >= MAX_INSPECT_DEPTH {
        return ("…".into(), "Value");
    }
    match raw {
        OvbRaw::Null => ("null".into(), "Null"),
        OvbRaw::Bool(value) => (value.to_string(), "Bool"),
        OvbRaw::Int(value) => (truncate(&value.to_string()), "Int"),
        OvbRaw::Float(bits) => (format_float(*bits), "Float"),
        OvbRaw::Bytes(bytes) => (inspect_bytes(bytes), "Bytes"),
        OvbRaw::Text(value) => (format!("\"{}\"", escape(value)), "Str"),
        OvbRaw::Array(values) => (inspect_sequence(values, depth), "Array"),
        OvbRaw::Map(entries) => (inspect_map(entries, depth), "Map"),
        OvbRaw::Tag(0 | 60011 | 60012 | 60016 | 60026, _) => ("<redacted>".into(), "Value"),
        OvbRaw::Tag(37, value) => inspect_uuid(value),
        OvbRaw::Tag(tag, value) => {
            let (text, _) = inspect_raw(value, depth + 1);
            (format!("Tag<{tag}>({text})"), "Tagged")
        }
    }
}

fn inspect_uuid(value: &OvbRaw) -> (String, &'static str) {
    let OvbRaw::Bytes(bytes) = value else {
        return ("Tag<37>(<invalid>)".into(), "Tagged");
    };
    let Ok(bytes) = <[u8; 16]>::try_from(bytes.as_slice()) else {
        return ("Tag<37>(<invalid>)".into(), "Tagged");
    };
    (canonical_uuid_text(bytes), "Uuid")
}

fn inspect_sequence(values: &[OvbRaw], depth: usize) -> String {
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

fn inspect_map(entries: &[(OvbRaw, OvbRaw)], depth: usize) -> String {
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
    let text = bytes
        .iter()
        .take(MAX_INSPECT_TEXT / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let suffix = if bytes.len() > MAX_INSPECT_TEXT / 2 {
        "…"
    } else {
        ""
    };
    format!("0x{text}{suffix}")
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

/// Native and wasm tests exercise the same exported boundary. Fixtures remain
/// inside this crate so the wasm proof never depends on a sibling reference tree.
#[cfg(test)]
mod tests {
    use super::{ReplSession, run};
    use serde_json::Value;
    use wasm_bindgen_test::wasm_bindgen_test;

    const REPL_SOURCE: &str = include_str!("../tests/fixtures/repl.orna");
    const INVALID_SOURCE: &str = include_str!("../tests/fixtures/invalid.orna");

    #[wasm_bindgen_test]
    fn run_returns_stable_values_and_stdout() {
        let result: Value = serde_json::from_str(&run(REPL_SOURCE)).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["values"][0], "42 : Int");
        assert_eq!(result["stdout"], "42 : Int\n");
        assert_eq!(result["errors"], serde_json::json!([]));
    }

    #[wasm_bindgen_test]
    fn run_keeps_error_locations_and_continues_after_a_bad_input() {
        let result: Value = serde_json::from_str(&run(INVALID_SOURCE)).unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["values"][0], "7 : Int");
        assert_eq!(result["errors"][0]["line"], 2);
        assert!(result["errors"][0]["col"].as_u64().unwrap() >= 1);
    }

    #[wasm_bindgen_test]
    fn repl_session_retains_bindings_between_calls() {
        let mut session = ReplSession::new();
        let declaration: Value =
            serde_json::from_str(&session.evaluate("let answer = 40")).unwrap();
        assert_eq!(declaration["kind"], "echo");
        let value: Value = serde_json::from_str(&session.evaluate("answer + 2")).unwrap();
        assert_eq!(value["kind"], "value");
        assert_eq!(value["text"], "42 : Int");
    }
}
