//! Thin browser-facing wrappers around the existing LSP analysis functions.
//!
//! The web worker owns transport and Monaco adapts LSP-shaped JSON to its
//! provider interfaces. Parsing and language features remain in `analysis`.

use lsp_types::{Position, Uri};
use serde::Serialize;

use crate::{
    analysis::{self, parse_document},
    documents::{Document, PositionMapper},
};

fn document(source: String) -> Document {
    Document::new(
        "file:///playground/main.orna"
            .parse::<Uri>()
            .expect("static browser document URI is valid"),
        source,
        1,
    )
}

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

/// Returns compiler diagnostics for a source document as an LSP JSON array.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn diagnostics(source: String) -> String {
    let document = document(source);
    let mapper = PositionMapper::new(&document.text);
    json(&analysis::check_document(&document, &mapper))
}

/// Returns completion items from the existing LSP completion implementation.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn completions(source: String, line: u32, character: u32) -> String {
    let document = document(source);
    let parsed = parse_document(&document);
    let items = analysis::completion_at(
        &parsed,
        &document.text,
        Some(PositionMapper::new(&document.text).byte_offset(Position { line, character })),
        None,
    );
    json(&items)
}

/// Returns hover information from the existing LSP hover implementation.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn hover(source: String, line: u32, character: u32) -> String {
    let document = document(source);
    let parsed = parse_document(&document);
    let mapper = PositionMapper::new(&document.text);
    json(&analysis::hover(
        &document,
        &parsed,
        Position { line, character },
        &mapper,
    ))
}

/// Returns signature help from the existing LSP signature implementation.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn signature_help(source: String, line: u32, character: u32) -> String {
    let document = document(source);
    let parsed = parse_document(&document);
    let mapper = PositionMapper::new(&document.text);
    json(&analysis::signature_help(
        &document,
        &parsed,
        Position { line, character },
        &mapper,
    ))
}

#[cfg(test)]
mod tests {
    use super::{completions, diagnostics, hover, signature_help};

    const SOURCE: &str = include_str!("browser/fixtures/browser-intelligence.orna");
    const STANDARD_SOURCE: &str = include_str!("analysis/fixtures/standard-math-import.orna");
    const RANKED_STANDARD_SOURCE: &str =
        include_str!("browser/fixtures/standard-completion-ranking.orna");
    const STANDARD_SIGNATURE_SOURCE: &str =
        include_str!("browser/fixtures/standard-signature-help.orna");

    fn position(source: &str, byte: usize) -> (u32, u32) {
        let prefix = &source[..byte];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let character = prefix
            .rsplit('\n')
            .next()
            .expect("current line")
            .chars()
            .count() as u32;
        (line, character)
    }

    #[test]
    fn browser_exports_reuse_the_lsp_diagnostics_completion_hover_and_signature_core() {
        let diagnostics: serde_json::Value =
            serde_json::from_str(&diagnostics(SOURCE.to_owned())).expect("diagnostics JSON");
        assert_eq!(diagnostics, serde_json::json!([]));

        let completions: serde_json::Value =
            serde_json::from_str(&completions(SOURCE.to_owned(), 0, 0)).expect("completion JSON");
        assert!(
            completions
                .as_array()
                .expect("completion list")
                .iter()
                .any(|item| item["label"] == "double")
        );

        let call_offset = SOURCE.find("double(2)").expect("call site") + 1;
        let call_prefix = &SOURCE[..call_offset];
        let line = call_prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let character = call_prefix
            .rsplit('\n')
            .next()
            .expect("current line")
            .chars()
            .count() as u32;

        let hover: serde_json::Value =
            serde_json::from_str(&hover(SOURCE.to_owned(), line, character)).expect("hover JSON");
        assert!(hover.to_string().contains("fn double(value: Int): Int"));

        let signature: serde_json::Value = serde_json::from_str(&signature_help(
            SOURCE.to_owned(),
            line,
            character + "double(".chars().count() as u32 - 1,
        ))
        .expect("signature JSON");
        assert!(signature.to_string().contains("fn double(value: Int): Int"));
    }

    #[test]
    fn browser_diagnostics_report_invalid_source_from_the_shared_parser() {
        let diagnostics: serde_json::Value = serde_json::from_str(&diagnostics(
            "pub fn broken(value: Int): Int = value + ;".to_owned(),
        ))
        .expect("diagnostics JSON");
        assert_eq!(diagnostics[0]["source"], "orna-syntax-v1");
        assert_eq!(diagnostics[0]["severity"], 1);
    }

    #[test]
    fn browser_exports_offer_standard_library_completions_and_hover_documentation() {
        let import_offset = STANDARD_SOURCE.find("increment};").expect("math import") + 3;
        let (line, character) = position(STANDARD_SOURCE, import_offset);
        let completion_items: serde_json::Value =
            serde_json::from_str(&completions(STANDARD_SOURCE.to_owned(), line, character))
                .expect("standard completion JSON");
        let increment = completion_items
            .as_array()
            .expect("completion list")
            .iter()
            .find(|item| item["label"] == "increment")
            .expect("standard-library completion");
        assert_eq!(increment["insertText"], "increment");
        assert!(
            increment["documentation"]
                .to_string()
                .contains("exact successor")
        );

        let qualified_offset = STANDARD_SOURCE
            .find("std.math.increment(value)")
            .expect("qualified standard call")
            + "std.math.inc".len();
        let (line, character) = position(STANDARD_SOURCE, qualified_offset);
        let qualified_completions: serde_json::Value =
            serde_json::from_str(&completions(STANDARD_SOURCE.to_owned(), line, character))
                .expect("qualified standard completion JSON");
        assert!(
            qualified_completions
                .as_array()
                .expect("completion list")
                .iter()
                .any(|item| {
                    item["label"] == "increment" && item["insertText"] == "increment(${1:value})"
                })
        );

        let hover_offset = STANDARD_SOURCE
            .find("std.math.increment(value)")
            .expect("qualified standard call")
            + "std.math.increment".len()
            - 1;
        let (line, character) = position(STANDARD_SOURCE, hover_offset);
        let hover: serde_json::Value =
            serde_json::from_str(&hover(STANDARD_SOURCE.to_owned(), line, character))
                .expect("standard hover JSON");
        assert!(hover.to_string().contains("fn increment(value: Int): Int"));
        assert!(hover.to_string().contains("exact successor"));
    }

    #[test]
    fn browser_standard_completion_ranking_and_hover_match_editor_contract() {
        let completion_offset = RANKED_STANDARD_SOURCE
            .find("increment(value)")
            .expect("standard call")
            + "increment".len();
        let (line, character) = position(RANKED_STANDARD_SOURCE, completion_offset);
        let completion_items: serde_json::Value = serde_json::from_str(&completions(
            RANKED_STANDARD_SOURCE.to_owned(),
            line,
            character,
        ))
        .expect("ranked standard completion JSON");
        let items = completion_items.as_array().expect("completion list");
        let labels = items
            .iter()
            .map(|item| item["label"].as_str().expect("completion label"))
            .collect::<Vec<_>>();
        assert_eq!(labels, ["increment", "incremental"]);
        assert_eq!(items[0]["sortText"], "0-1-increment");
        assert_eq!(items[0]["preselect"], true);
        assert_eq!(items[1]["sortText"], "1-1-incremental");

        let hover_offset = RANKED_STANDARD_SOURCE
            .find("increment(value)")
            .expect("standard call")
            + "increment".len()
            - 1;
        let (line, character) = position(RANKED_STANDARD_SOURCE, hover_offset);
        let hover: serde_json::Value =
            serde_json::from_str(&hover(RANKED_STANDARD_SOURCE.to_owned(), line, character))
                .expect("standard hover JSON");
        assert!(hover.to_string().contains("fn increment(value: Int): Int"));
        assert!(hover.to_string().contains("exact successor"));
    }

    #[test]
    fn browser_standard_signature_help_exposes_parameter_hints_and_active_argument() {
        let call_offset = STANDARD_SIGNATURE_SOURCE
            .find("clamp(value, lower, upper)")
            .expect("standard clamp call");
        let active_argument_offset = call_offset + "clamp(value, lower, ".len();
        let (line, character) = position(STANDARD_SIGNATURE_SOURCE, active_argument_offset);
        let signature: serde_json::Value = serde_json::from_str(&signature_help(
            STANDARD_SIGNATURE_SOURCE.to_owned(),
            line,
            character,
        ))
        .expect("standard signature help JSON");

        assert_eq!(signature["activeSignature"], 0);
        assert_eq!(signature["activeParameter"], 2);
        assert_eq!(signature["signatures"].as_array().map(Vec::len), Some(1));
        let standard_signature = &signature["signatures"][0];
        assert_eq!(
            standard_signature["label"],
            "fn clamp(value: Int, lower: Int, upper: Int): Int"
        );
        let parameters = standard_signature["parameters"]
            .as_array()
            .expect("parameter hints");
        assert_eq!(parameters.len(), 3);
        assert_eq!(parameters[0]["label"], "value: Int");
        assert_eq!(parameters[1]["label"], "lower: Int");
        assert_eq!(parameters[2]["label"], "upper: Int");
    }
}
