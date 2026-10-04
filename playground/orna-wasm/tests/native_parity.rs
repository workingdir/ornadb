#![cfg(not(target_arch = "wasm32"))]

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_syntax_v1::parse_repl;
use orna_value_v1::Raw;
use serde_json::Value;

use orna_wasm::run;

const PLAYGROUND_ARITHMETIC: &str = include_str!("fixtures/playground-arithmetic.orna");
const PLAYGROUND_MULTILINE: &str = include_str!("fixtures/playground-multiline.orna");
const PLAYGROUND_VALUES: &str = include_str!("fixtures/playground-values.orna");

#[test]
fn wasm_run_sample_results_match_direct_native_evaluator_results() {
    let samples = [
        (PLAYGROUND_ARITHMETIC, vec!["42 : Int"]),
        (PLAYGROUND_MULTILINE, vec!["42 : Int", "43 : Int"]),
        (PLAYGROUND_VALUES, vec!["true : Bool", "\"Orna\" : Str"]),
    ];

    for (source, expected_values) in samples {
        let native_values = evaluate_native(source);
        assert_eq!(native_values, expected_values);

        let wasm_contract: Value = serde_json::from_str(&run(source)).expect("run JSON");
        assert_eq!(wasm_contract["ok"], true);
        assert_eq!(wasm_contract["values"], serde_json::json!(expected_values));
        assert_eq!(wasm_contract["stdout"], "");
        assert_eq!(wasm_contract["errors"], serde_json::json!([]));
        assert!(
            wasm_contract["ast"]
                .as_str()
                .is_some_and(|ast| !ast.is_empty())
        );
    }
}

fn evaluate_native(source: &str) -> Vec<String> {
    let mut session = AdmittedReplSession::new(Limits::default());
    let mut pending = String::new();
    let mut values = Vec::new();

    for line in source.split_inclusive('\n') {
        pending.push_str(line);
        let parsed = parse_repl(&pending);
        if parsed.is_incomplete() {
            continue;
        }
        assert!(parsed.is_ok(), "fixture failed to parse: {pending}");
        if let Some(value) = session
            .submit(&pending)
            .unwrap_or_else(|error| panic!("native fixture failed: {}", error.code()))
        {
            values.push(render_native_value(value.raw()));
        }
        pending.clear();
    }

    if !pending.is_empty() {
        let parsed = parse_repl(&pending);
        assert!(
            parsed.is_ok(),
            "fixture ended with an incomplete input: {pending}"
        );
        if let Some(value) = session
            .submit(&pending)
            .unwrap_or_else(|error| panic!("native fixture failed: {}", error.code()))
        {
            values.push(render_native_value(value.raw()));
        }
    }

    values
}

fn render_native_value(raw: &Raw) -> String {
    match raw {
        Raw::Null => "null : Null".to_owned(),
        Raw::Bool(value) => format!("{value} : Bool"),
        Raw::Int(value) => format!("{value} : Int"),
        Raw::Text(value) => format!("\"{}\" : Str", value.escape_default()),
        other => panic!("unexpected sample result: {other:?}"),
    }
}
