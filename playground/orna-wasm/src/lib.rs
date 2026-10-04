//! WebAssembly boundary for the Orna playground.
//!
//! `run` is the stable, stateless JSON contract consumed by the interactive
//! wrapper. The wrapper rebuilds a candidate session through that boundary
//! before publishing each completed input, so an error never commits a partial
//! declaration to the browser's REPL history.

use std::fmt::Write as _;

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{ReplInput, parse_repl, parse_repl_with_file};
use orna_value_v1::Raw;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

const MAX_RENDER_DEPTH: usize = 4;
const MAX_RENDER_ITEMS: usize = 16;
const MAX_RENDER_TEXT: usize = 256;

#[derive(Debug, Deserialize, Serialize)]
struct RunError {
    message: String,
    line: usize,
    col: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct RunResponse {
    ok: bool,
    values: Vec<String>,
    stdout: String,
    errors: Vec<RunError>,
}

impl RunResponse {
    fn success(values: Vec<String>) -> Self {
        Self {
            ok: true,
            values,
            stdout: String::new(),
            errors: Vec::new(),
        }
    }

    fn failure(values: Vec<String>, message: impl Into<String>, line: usize, col: usize) -> Self {
        Self {
            ok: false,
            values,
            stdout: String::new(),
            errors: vec![RunError {
                message: message.into(),
                line,
                col,
            }],
        }
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            "{\"ok\":false,\"values\":[],\"stdout\":\"\",\"errors\":[{\"message\":\"ORNA-WASM-JSON\",\"line\":1,\"col\":1}]}".into()
        })
    }
}

/// Evaluates a complete source string with the playground's stable JSON shape.
///
/// Input is handled as a sequence of REPL statements. Incomplete statements
/// continue across physical lines; a final incomplete statement is reported
/// with its opening line and first non-whitespace column.
#[wasm_bindgen]
pub fn run(source: &str) -> String {
    run_internal(source).to_json()
}

fn run_internal(source: &str) -> RunResponse {
    let limits = Limits::default();
    if source.len() > limits.max_source_bytes {
        return RunResponse::failure(Vec::new(), "ORNA-EVAL-LIMIT", 1, 1);
    }

    let mut session = AdmittedReplSession::new(limits);
    let mut values = Vec::new();
    let mut pending = String::new();
    let mut pending_line = 1;
    let mut next_line = 1;

    for physical_line in source.split_inclusive('\n') {
        if pending.is_empty() && physical_line.trim().is_empty() {
            next_line += line_count(physical_line);
            continue;
        }
        if pending.is_empty() {
            pending_line = next_line;
        }
        pending.push_str(physical_line);
        next_line += line_count(physical_line);

        let parsed = parse_repl(&pending);
        if parsed.is_incomplete() {
            continue;
        }
        if !parsed.is_ok() {
            let located = parse_repl_with_file(&pending, "<playground>");
            let diagnostic = located.diagnostics.first();
            let (line, col) = diagnostic
                .and_then(|diagnostic| diagnostic.span.start_position.as_ref())
                .map_or((pending_line, first_column(&pending)), |position| {
                    (
                        pending_line + position.line as usize - 1,
                        position.column as usize,
                    )
                });
            return RunResponse::failure(
                values,
                diagnostic.map_or("ORNA-PARSE-001", |diagnostic| diagnostic.message.as_str()),
                line,
                col,
            );
        }

        match session.submit(&pending) {
            Ok(Some(value)) => values.push(render_value(&value)),
            Ok(None) => {}
            Err(error) => {
                return RunResponse::failure(
                    values,
                    error.code(),
                    pending_line,
                    first_column(&pending),
                );
            }
        }
        pending.clear();
    }

    if !pending.is_empty() {
        let parsed = parse_repl(&pending);
        if parsed.is_incomplete() {
            return RunResponse::failure(
                values,
                "ORNA-EVAL-INCOMPLETE",
                pending_line,
                first_column(&pending),
            );
        }
        // `split_inclusive` omits the empty final segment, so a non-incomplete
        // final submission has already been evaluated in the loop above.
    }

    RunResponse::success(values)
}

fn line_count(line: &str) -> usize {
    line.bytes().filter(|byte| *byte == b'\n').count().max(1)
}

fn first_column(source: &str) -> usize {
    source
        .chars()
        .take_while(|scalar| scalar.is_whitespace() && *scalar != '\n')
        .count()
        + 1
}

/// Stateful browser-facing wrapper for incremental input and statement echo.
#[wasm_bindgen]
pub struct ReplSession {
    history: String,
    pending: String,
    value_count: usize,
}

#[wasm_bindgen]
impl ReplSession {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            history: String::new(),
            pending: String::new(),
            value_count: 0,
        }
    }

    /// Adds one physical input line and returns `{kind, text}` JSON.
    ///
    /// Blank top-level lines echo empty text and remain in session source
    /// coordinates; blank lines inside an incomplete input remain pending.
    /// Completed parse and evaluation errors use the locations from `run`.
    pub fn evaluate(&mut self, line: &str) -> String {
        self.evaluate_using(line, &mut run)
    }
}

impl Default for ReplSession {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplSession {
    fn evaluate_using(&mut self, line: &str, runner: &mut impl FnMut(&str) -> String) -> String {
        if self.pending.is_empty() && line.trim().is_empty() {
            self.history.push_str(line);
            self.history.push('\n');
            return response("echo", "");
        }

        self.pending.push_str(line);
        self.pending.push('\n');

        let parsed = parse_repl(&self.pending);
        if parsed.is_incomplete() {
            return response("echo", self.pending.trim_end());
        }
        let item = parsed.is_ok() && matches!(&parsed.value, ReplInput::Item(_));
        let candidate = format!("{}{}", self.history, self.pending);
        let result = runner(&candidate);
        let Ok(result) = serde_json::from_str::<RunResponse>(&result) else {
            self.pending.clear();
            return response("error", "ORNA-WASM-CONTRACT");
        };
        if !result.ok {
            let text = run_error_text(&result);
            self.pending.clear();
            return response("error", &text);
        }
        if !parsed.is_ok() {
            self.pending.clear();
            return response("error", "ORNA-WASM-CONTRACT");
        }

        let has_new_value = !item && result.values.len() > self.value_count;
        let text = if item {
            self.pending.trim().to_owned()
        } else if has_new_value {
            result.values.last().cloned().unwrap_or_default()
        } else {
            self.pending.trim().to_owned()
        };
        self.history.push_str(&self.pending);
        self.pending.clear();
        self.value_count = result.values.len();
        response(
            if item {
                "echo"
            } else if has_new_value {
                "value"
            } else {
                "echo"
            },
            &text,
        )
    }
}

fn response(kind: &str, text: &str) -> String {
    #[derive(Serialize)]
    struct ReplResponse<'a> {
        kind: &'a str,
        text: &'a str,
    }
    serde_json::to_string(&ReplResponse { kind, text })
        .unwrap_or_else(|_| "{\"kind\":\"error\",\"text\":\"ORNA-WASM-JSON\"}".into())
}

fn run_error_text(result: &RunResponse) -> String {
    result.errors.first().map_or_else(
        || "ORNA-WASM-RUN".to_owned(),
        |error| format!("{} (line {}, col {})", error.message, error.line, error.col),
    )
}

fn render_value(value: &CanonicalValue) -> String {
    let (text, ty) = inspect_raw(value.raw(), 0);
    format!("{text} : {ty}")
}

fn inspect_raw(raw: &Raw, depth: usize) -> (String, &'static str) {
    if depth >= MAX_RENDER_DEPTH {
        return ("…".into(), "Value");
    }
    match raw {
        Raw::Null => ("null".into(), "Null"),
        Raw::Bool(value) => (value.to_string(), "Bool"),
        Raw::Int(value) => (truncate(&value.to_string()), "Int"),
        Raw::Float(bits) => (format_float(*bits), "Float"),
        Raw::Bytes(bytes) => (inspect_bytes(bytes), "Bytes"),
        Raw::Text(value) => (inspect_text(value), "Str"),
        Raw::Array(values) => (inspect_sequence(values, depth), "Array"),
        Raw::Map(entries) => (inspect_map(entries, depth), "Map"),
        Raw::Tag(0 | 60011 | 60012 | 60016 | 60026, _) => ("<redacted>".into(), "Secret"),
        Raw::Tag(37, value) => inspect_uuid(value),
        Raw::Tag(60000, value) => inspect_decimal(value),
        Raw::Tag(60001, value) => inspect_typed_text(value, "Date"),
        Raw::Tag(60002, value) => inspect_typed_payload(value, depth, "Instant"),
        Raw::Tag(60003, value) => inspect_typed_text(value, "LocalDateTime"),
        Raw::Tag(60004, value) => inspect_typed_text(value, "TimeZone"),
        Raw::Tag(60005, value) => inspect_typed_payload(value, depth, "Duration"),
        Raw::Tag(60006, value) => inspect_typed_payload(value, depth, "Quantity"),
        Raw::Tag(60007, value) => inspect_typed_payload(value, depth, "Money"),
        Raw::Tag(60008, value) => inspect_typed_payload(value, depth, "ZonedDateTime"),
        Raw::Tag(60017, value) => inspect_typed_payload(value, depth, "TimeOfDay"),
        Raw::Tag(60018, value) => inspect_typed_payload(value, depth, "ZonedDateTime"),
        Raw::Tag(60019, value) => inspect_typed_payload(value, depth, "Range"),
        Raw::Tag(tag, value) => {
            let (text, _) = inspect_raw(value, depth + 1);
            (format!("Tag<{tag}>({text})"), "Tagged")
        }
    }
}

fn inspect_text(value: &str) -> String {
    let mut escaped = String::new();
    for scalar in value.chars() {
        let start = escaped.len();
        match scalar {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\0' => escaped.push_str("\\0"),
            scalar if scalar.is_control() => {
                write!(&mut escaped, "\\u{{{:x}}}", scalar as u32)
                    .expect("writing to String is infallible");
            }
            scalar => escaped.push(scalar),
        }
        if escaped.len() > MAX_RENDER_TEXT {
            escaped.truncate(start);
            escaped.push('…');
            break;
        }
    }
    format!("\"{escaped}\"")
}

fn inspect_decimal(value: &Raw) -> (String, &'static str) {
    let Raw::Array(fields) = value else {
        return ("<invalid>".into(), "Decimal");
    };
    let [Raw::Int(coefficient), Raw::Int(exponent)] = fields.as_slice() else {
        return ("<invalid>".into(), "Decimal");
    };
    (format!("{coefficient}e{exponent}.decimal"), "Decimal")
}

fn inspect_typed_text(value: &Raw, ty: &'static str) -> (String, &'static str) {
    match value {
        Raw::Text(text) => (inspect_text(text), ty),
        _ => ("<invalid>".into(), ty),
    }
}

fn inspect_typed_payload(value: &Raw, depth: usize, ty: &'static str) -> (String, &'static str) {
    let (text, _) = inspect_raw(value, depth + 1);
    (format!("{ty}({text})"), ty)
}

fn inspect_uuid(value: &Raw) -> (String, &'static str) {
    let Raw::Bytes(bytes) = value else {
        return ("Tag<37>(<invalid>)".into(), "Tagged");
    };
    let Ok(bytes) = <[u8; 16]>::try_from(bytes.as_slice()) else {
        return ("Tag<37>(<invalid>)".into(), "Tagged");
    };
    (orna_foundation_v1::canonical_uuid_text(bytes), "Uuid")
}

fn inspect_sequence(values: &[Raw], depth: usize) -> String {
    let mut items = values
        .iter()
        .take(MAX_RENDER_ITEMS)
        .map(|value| inspect_raw(value, depth + 1).0)
        .collect::<Vec<_>>();
    if values.len() > MAX_RENDER_ITEMS {
        items.push("…".into());
    }
    format!("[{}]", items.join(", "))
}

fn inspect_map(entries: &[(Raw, Raw)], depth: usize) -> String {
    let mut items = entries
        .iter()
        .take(MAX_RENDER_ITEMS)
        .map(|(key, value)| {
            let key = inspect_raw(key, depth + 1).0;
            let value = inspect_raw(value, depth + 1).0;
            format!("{key}: {value}")
        })
        .collect::<Vec<_>>();
    if entries.len() > MAX_RENDER_ITEMS {
        items.push("…".into());
    }
    format!("{{{}}}", items.join(", "))
}

fn inspect_bytes(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in bytes.iter().take(MAX_RENDER_TEXT / 2) {
        write!(&mut text, "{byte:02x}").expect("writing to String is infallible");
    }
    if bytes.len() > MAX_RENDER_TEXT / 2 {
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

fn truncate(value: &str) -> String {
    let mut end = value.len().min(MAX_RENDER_TEXT);
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
    use serde_json::Value as JsonValue;

    const REPL_FIXTURE: &str = include_str!("../tests/fixtures/repl_incremental.orna");
    const REPL_MULTILINE_FIXTURE: &str = include_str!("../tests/fixtures/repl_multiline.orna");
    const REPL_RENDERING_FIXTURE: &str = include_str!("../tests/fixtures/repl_rendering.orna");
    const REPL_UNICODE_PARSE_ERROR_FIXTURE: &str =
        include_str!("../tests/fixtures/repl_unicode_parse_error.orna");
    const REPL_MULTILINE_PARSE_ERROR_FIXTURE: &str =
        include_str!("../tests/fixtures/repl_multiline_parse_error.orna");
    const REPL_RUNTIME_ERROR_FIXTURE: &str =
        include_str!("../tests/fixtures/repl_runtime_error.orna");
    const REPL_LOCATED_RUNTIME_ERROR_FIXTURE: &str =
        include_str!("../tests/fixtures/repl_located_runtime_error.orna");

    #[test]
    fn run_returns_values_in_stable_json_shape() {
        let actual: JsonValue = serde_json::from_str(&run(REPL_FIXTURE)).expect("valid JSON");
        assert_eq!(actual["ok"], true);
        assert_eq!(actual["values"], serde_json::json!(["42 : Int"]));
        assert_eq!(actual["stdout"], "");
        assert_eq!(actual["errors"], serde_json::json!([]));
    }

    #[test]
    fn run_reports_parse_and_runtime_error_locations() {
        let parsed: RunResponse = serde_json::from_str(&run("1 + )")).expect("valid JSON");
        assert!(!parsed.ok);
        assert_eq!((parsed.errors[0].line, parsed.errors[0].col), (1, 5));

        let runtime: RunResponse =
            serde_json::from_str(&run("let x = 1;\nmissing")).expect("valid JSON");
        assert!(!runtime.ok);
        assert_eq!(runtime.errors[0].message, "ORNA-S012-UNRESOLVED");
        assert_eq!((runtime.errors[0].line, runtime.errors[0].col), (2, 1));
    }

    #[test]
    fn repl_echoes_statements_and_renders_incremental_values() {
        let mut repl = ReplSession::new();
        assert_eq!(
            json(&repl.evaluate("let answer = 40;")),
            serde_json::json!({"kind":"echo","text":"let answer = 40;"})
        );
        assert_eq!(
            json(&repl.evaluate("answer + 2")),
            serde_json::json!({"kind":"value","text":"42 : Int"})
        );
    }

    #[test]
    fn repl_holds_incomplete_input_until_the_expression_closes() {
        let mut repl = ReplSession::new();
        assert_eq!(json(&repl.evaluate("1 +"))["kind"], "echo");
        assert_eq!(
            json(&repl.evaluate("2")),
            serde_json::json!({"kind":"value","text":"3 : Int"})
        );
    }

    #[test]
    fn repl_keeps_blank_lines_inside_incomplete_multiline_input() {
        let mut repl = ReplSession::new();
        assert_eq!(
            json(&repl.evaluate("1 +")),
            serde_json::json!({"kind":"echo","text":"1 +"})
        );
        assert_eq!(
            json(&repl.evaluate("")),
            serde_json::json!({"kind":"echo","text":"1 +"})
        );
        assert_eq!(
            json(&repl.evaluate("2")),
            serde_json::json!({"kind":"value","text":"3 : Int"})
        );
    }

    #[test]
    fn repl_multiline_values_match_the_run_contract() {
        let expected: RunResponse =
            serde_json::from_str(&run(REPL_MULTILINE_FIXTURE)).expect("valid run JSON");
        assert!(expected.ok);

        let mut repl = ReplSession::new();
        let actual = REPL_MULTILINE_FIXTURE
            .lines()
            .map(|line| json(&repl.evaluate(line)))
            .collect::<Vec<_>>();
        assert_eq!(
            JsonValue::Array(actual.clone()),
            serde_json::json!([
                {"kind":"echo","text":"let answer = 40;"},
                {"kind":"echo","text":"answer +"},
                {"kind":"value","text":"42 : Int"},
                {"kind":"echo","text":"answer +"},
                {"kind":"value","text":"43 : Int"}
            ])
        );
        let values = actual
            .iter()
            .filter(|actual| actual["kind"] == "value")
            .map(|actual| actual["text"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(values, expected.values);
        assert_eq!(values, ["42 : Int", "43 : Int"]);
    }

    #[test]
    fn run_and_repl_preserve_escaped_text_and_registered_value_types() {
        let expected: RunResponse =
            serde_json::from_str(&run(REPL_RENDERING_FIXTURE)).expect("valid run JSON");
        assert!(expected.ok);
        assert_eq!(
            expected.values[0],
            r#""quote: \" slash: \\ newline: \n tab: \t" : Str"#
        );
        assert_eq!(
            expected.values[1],
            r#"["café", "😀", 7, true, null] : Array"#
        );
        assert_eq!(expected.values[2], r#""2026-09-01" : Date"#);
        assert!(expected.values[3].ends_with(" : Instant"));
        assert_eq!(expected.values[4], "123e0.decimal : Decimal");
        assert!(expected.values[5].ends_with(" : Range"));

        let mut repl = ReplSession::new();
        let actual = REPL_RENDERING_FIXTURE
            .lines()
            .map(|line| json(&repl.evaluate(line)))
            .filter(|response| response["kind"] == "value")
            .map(|response| response["text"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected.values);
    }

    #[test]
    fn text_rendering_escapes_whole_scalars_before_truncating() {
        assert_eq!(
            inspect_text("quote\" slash\\ newline\n tab\t nul\0 '"),
            "\"quote\\\" slash\\\\ newline\\n tab\\t nul\\0 '\""
        );
        assert_eq!(
            inspect_text(&"\n".repeat(129)),
            format!("\"{}…\"", "\\n".repeat(128))
        );
    }

    #[test]
    fn protected_tag_payloads_stay_redacted_in_nested_values() {
        for tag in [0, 60011, 60012, 60016, 60026] {
            let value = Raw::Array(vec![Raw::Tag(tag, Box::new(Raw::Text("sensitive".into())))]);
            let (text, ty) = inspect_raw(&value, 0);
            assert_eq!(text, "[<redacted>]");
            assert_eq!(ty, "Array");
            assert!(!text.contains("sensitive"));
        }
    }

    #[test]
    fn repl_multiline_parse_errors_match_run_locations_and_recover() {
        let expected: RunResponse =
            serde_json::from_str(&run(REPL_MULTILINE_PARSE_ERROR_FIXTURE)).expect("valid run JSON");
        assert!(!expected.ok);
        assert_eq!((expected.errors[0].line, expected.errors[0].col), (4, 1));
        let expected_error = run_error_text(&expected);

        let mut repl = ReplSession::new();
        let lines = REPL_MULTILINE_PARSE_ERROR_FIXTURE
            .lines()
            .collect::<Vec<_>>();
        assert_eq!(json(&repl.evaluate(lines[0]))["kind"], "echo");
        assert_eq!(
            json(&repl.evaluate(lines[1])),
            serde_json::json!({"kind":"echo","text":""})
        );
        assert_eq!(json(&repl.evaluate(lines[2]))["kind"], "echo");
        assert_eq!(
            json(&repl.evaluate(lines[3])),
            serde_json::json!({"kind":"error","text":expected_error})
        );
        assert_eq!(
            json(&repl.evaluate("answer + 2")),
            serde_json::json!({"kind":"value","text":"42 : Int"})
        );
    }

    #[test]
    fn repl_unicode_parse_error_columns_match_run_and_recover() {
        let expected: RunResponse =
            serde_json::from_str(&run(REPL_UNICODE_PARSE_ERROR_FIXTURE)).expect("valid run JSON");
        assert!(!expected.ok);
        assert_eq!((expected.errors[0].line, expected.errors[0].col), (3, 9));
        let expected_error = run_error_text(&expected);

        let mut repl = ReplSession::new();
        let lines = REPL_UNICODE_PARSE_ERROR_FIXTURE.lines().collect::<Vec<_>>();
        assert_eq!(json(&repl.evaluate(lines[0]))["kind"], "echo");
        assert_eq!(
            json(&repl.evaluate(lines[1])),
            serde_json::json!({"kind":"echo","text":""})
        );
        assert_eq!(
            json(&repl.evaluate(lines[2])),
            serde_json::json!({"kind":"error","text":expected_error})
        );
        assert_eq!(
            json(&repl.evaluate("answer + 2")),
            serde_json::json!({"kind":"value","text":"42 : Int"})
        );
    }

    #[test]
    fn repl_runtime_error_columns_match_run_after_blank_lines() {
        let expected: RunResponse =
            serde_json::from_str(&run(REPL_LOCATED_RUNTIME_ERROR_FIXTURE)).expect("valid run JSON");
        assert!(!expected.ok);
        assert_eq!((expected.errors[0].line, expected.errors[0].col), (3, 4));
        let expected_error = run_error_text(&expected);

        let mut repl = ReplSession::new();
        let lines = REPL_LOCATED_RUNTIME_ERROR_FIXTURE
            .lines()
            .collect::<Vec<_>>();
        assert_eq!(json(&repl.evaluate(lines[0]))["kind"], "echo");
        assert_eq!(
            json(&repl.evaluate(lines[1])),
            serde_json::json!({"kind":"echo","text":""})
        );
        assert_eq!(
            json(&repl.evaluate(lines[2])),
            serde_json::json!({"kind":"error","text":expected_error})
        );
        assert_eq!(
            json(&repl.evaluate("answer + 2")),
            serde_json::json!({"kind":"value","text":"42 : Int"})
        );
    }

    #[test]
    fn tab_indentation_uses_the_same_scalar_column_in_run_and_repl() {
        let source = "\tmissing";
        let expected: RunResponse = serde_json::from_str(&run(source)).expect("valid run JSON");
        assert!(!expected.ok);
        assert_eq!((expected.errors[0].line, expected.errors[0].col), (1, 2));

        let mut repl = ReplSession::new();
        assert_eq!(
            json(&repl.evaluate(source)),
            serde_json::json!({
                "kind":"error",
                "text":run_error_text(&expected)
            })
        );
    }

    #[test]
    fn repl_runtime_errors_match_run_locations_and_recover() {
        let expected: RunResponse =
            serde_json::from_str(&run(REPL_RUNTIME_ERROR_FIXTURE)).expect("valid run JSON");
        assert!(!expected.ok);
        let expected_error = run_error_text(&expected);

        let mut repl = ReplSession::new();
        let lines = REPL_RUNTIME_ERROR_FIXTURE.lines().collect::<Vec<_>>();
        assert_eq!(json(&repl.evaluate(lines[0]))["kind"], "echo");
        assert_eq!(
            json(&repl.evaluate(lines[1])),
            serde_json::json!({"kind":"error","text":expected_error})
        );
        assert_eq!(
            json(&repl.evaluate("answer + 2")),
            serde_json::json!({"kind":"value","text":"42 : Int"})
        );
    }

    #[test]
    fn repl_does_not_commit_failed_input_and_recovers_on_the_next_line() {
        let mut repl = ReplSession::new();
        let error = json(&repl.evaluate("missing"));
        assert_eq!(error["kind"], "error");
        assert_eq!(error["text"], "ORNA-S012-UNRESOLVED (line 1, col 1)");
        assert_eq!(
            json(&repl.evaluate("40 + 2")),
            serde_json::json!({"kind":"value","text":"42 : Int"})
        );
    }

    #[cfg(feature = "test-stub-run")]
    #[test]
    fn test_stub_run_feature_can_prove_the_json_runner_boundary() {
        let mut repl = ReplSession::new();
        let mut stub = |source: &str| {
            if source.contains("broken") {
                RunResponse::failure(Vec::new(), "stub failure", 1, 1).to_json()
            } else {
                RunResponse::success(vec!["stubbed : Int".into()]).to_json()
            }
        };
        let result = repl.evaluate_using("7", &mut stub);
        assert_eq!(json(&result)["kind"], "value");
        assert_eq!(json(&result)["text"], "stubbed : Int");
    }

    fn json(source: &str) -> JsonValue {
        serde_json::from_str(source).expect("valid REPL JSON")
    }
}
