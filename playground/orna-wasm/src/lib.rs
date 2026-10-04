use orna_evaluator_v1::{Limits, ReplSession as CoreReplSession};
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use serde::Serialize;
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
struct RunError {
    message: String,
    line: usize,
    col: usize,
}

#[derive(Serialize)]
struct RunResult {
    ok: bool,
    values: Vec<String>,
    stdout: String,
    errors: Vec<RunError>,
}

#[derive(Serialize)]
struct ReplResult {
    kind: &'static str,
    text: String,
}

/// Evaluates source one REPL input per non-empty line and returns stable JSON.
#[wasm_bindgen]
pub fn run(source: &str) -> String {
    let mut session = ReplSession::new();
    let mut values = Vec::new();
    let mut errors = Vec::new();

    for (line_index, line) in source.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match session.submit(line) {
            Ok(Some(value)) => values.push(display_value(&value)),
            Ok(None) => {}
            Err(message) => errors.push(RunError {
                message,
                line: line_index + 1,
                col: 1,
            }),
        }
    }

    json(&RunResult {
        ok: errors.is_empty(),
        values,
        stdout: String::new(),
        errors,
    })
}

/// An isolated, bounded REPL session for the browser playground.
#[wasm_bindgen]
pub struct ReplSession {
    inner: CoreReplSession,
}

#[wasm_bindgen]
impl ReplSession {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let limits = Limits::default();
        let inner = CoreReplSession::new(limits);
        Self { inner }
    }

    /// Evaluates one line and returns `{kind, text}` as JSON.
    pub fn evaluate(&mut self, line: &str) -> String {
        let result = match self.submit(line) {
            Ok(Some(value)) => ReplResult {
                kind: "value",
                text: display_value(&value),
            },
            Ok(None) => ReplResult {
                kind: "echo",
                text: line.trim().to_owned(),
            },
            Err(message) => ReplResult {
                kind: "error",
                text: message,
            },
        };
        json(&result)
    }
}

impl ReplSession {
    fn submit(&mut self, line: &str) -> Result<Option<CanonicalValue>, String> {
        self.inner
            .submit(line)
            .map_err(|error| error.code().to_owned())
    }
}

impl Default for ReplSession {
    fn default() -> Self {
        Self::new()
    }
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("playground JSON contains serializable values")
}

fn display_value(value: &CanonicalValue) -> String {
    display_raw(value.raw())
}

fn display_raw(value: &OvbRaw) -> String {
    match value {
        OvbRaw::Null => "null".to_owned(),
        OvbRaw::Bool(value) => value.to_string(),
        OvbRaw::Int(value) => value.to_string(),
        OvbRaw::Float(bits) => f64::from_bits(*bits).to_string(),
        OvbRaw::Text(value) => serde_json::to_string(value).expect("string JSON is valid"),
        OvbRaw::Array(values) => format!(
            "[{}]",
            values.iter().map(display_raw).collect::<Vec<_>>().join(", ")
        ),
        OvbRaw::Map(entries) => format!("{{{} entries}}", entries.len()),
        OvbRaw::Bytes(bytes) => format!("<{} bytes>", bytes.len()),
        OvbRaw::Tag(60014, _) => "()".to_owned(),
        OvbRaw::Tag(tag, _) => format!("<Orna value tag {tag}>"),
    }
}
