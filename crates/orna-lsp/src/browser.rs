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
        let completions: serde_json::Value =
            serde_json::from_str(&completions(STANDARD_SOURCE.to_owned(), line, character))
                .expect("standard completion JSON");
        let increment = completions
            .as_array()
            .expect("completion list")
            .iter()
            .find(|item| item["label"] == "increment")
            .expect("standard-library completion");
        assert_eq!(increment["insertText"], "increment(${1:value})");
        assert!(
            increment["documentation"]
                .to_string()
                .contains("exact successor")
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
}
