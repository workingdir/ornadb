use serde_json::Value;
use wasm_bindgen_test::wasm_bindgen_test;

use orna_wasm::{ReplSession, run};

const REPL_SOURCE: &str = include_str!("fixtures/repl-smoke.orna");
const PLAYGROUND_ARITHMETIC: &str = include_str!("fixtures/playground-arithmetic.orna");
const PLAYGROUND_MULTILINE: &str = include_str!("fixtures/playground-multiline.orna");
const PLAYGROUND_VALUES: &str = include_str!("fixtures/playground-values.orna");

#[wasm_bindgen_test]
fn run_returns_the_shared_json_contract() {
    let result: Value = serde_json::from_str(&run(REPL_SOURCE)).expect("run JSON");

    assert_eq!(result["ok"], true);
    assert_eq!(result["values"], serde_json::json!(["42 : Int"]));
    assert_eq!(result["stdout"], "");
    assert_eq!(result["errors"], serde_json::json!([]));
    assert!(result["ast"].as_str().is_some_and(|ast| !ast.is_empty()));
}

#[wasm_bindgen_test]
fn browser_sample_programs_match_the_native_parity_values() {
    let samples = [
        (PLAYGROUND_ARITHMETIC, serde_json::json!(["42 : Int"])),
        (
            PLAYGROUND_MULTILINE,
            serde_json::json!(["42 : Int", "43 : Int"]),
        ),
        (
            PLAYGROUND_VALUES,
            serde_json::json!(["true : Bool", "\"Orna\" : Str"]),
        ),
    ];

    for (source, expected_values) in samples {
        let result: Value = serde_json::from_str(&run(source)).expect("run JSON");
        assert_eq!(result["ok"], true);
        assert_eq!(result["values"], expected_values);
        assert_eq!(result["stdout"], "");
        assert_eq!(result["errors"], serde_json::json!([]));
        assert!(result["ast"].as_str().is_some_and(|ast| !ast.is_empty()));
    }
}

#[wasm_bindgen_test]
fn repl_retains_bindings_and_serializes_each_result_kind() {
    let mut repl = ReplSession::new();
    let echo: Value = serde_json::from_str(&repl.evaluate("let answer = 40;")).expect("echo JSON");
    let value: Value = serde_json::from_str(&repl.evaluate("answer + 2")).expect("value JSON");
    let error: Value = serde_json::from_str(&repl.evaluate("answer + true")).expect("error JSON");

    assert_eq!(
        echo,
        serde_json::json!({"kind": "echo", "text": "let answer = 40;"})
    );
    assert_eq!(
        value,
        serde_json::json!({"kind": "value", "text": "42 : Int"})
    );
    assert_eq!(error["kind"], "error");
    assert!(error["text"].as_str().is_some_and(|text| !text.is_empty()));
}
