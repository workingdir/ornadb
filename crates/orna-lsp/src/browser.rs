//! Thin browser-facing wrappers around the existing LSP analysis functions.
//!
//! The web worker owns transport and Monaco adapts LSP-shaped JSON to its
//! provider interfaces. Parsing and language features remain in `analysis`.

use lsp_types::{Position, Range, Uri};
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

/// Returns document symbols and their nested outline as an LSP JSON array.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn document_symbols(source: String) -> String {
    let document = document(source);
    let parsed = parse_document(&document);
    let mapper = PositionMapper::new(&document.text);
    json(&analysis::document_symbols(&parsed, &document.text, &mapper))
}

/// Returns inlay hints for the requested document range as an LSP JSON array.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn inlay_hints(
    source: String,
    start_line: u32,
    start_character: u32,
    end_line: u32,
    end_character: u32,
) -> String {
    let document = document(source);
    let parsed = parse_document(&document);
    let mapper = PositionMapper::new(&document.text);
    json(&crate::inlay::inlay_hints(
        &parsed,
        &document.text,
        &mapper,
        &Range {
            start: Position {
                line: start_line,
                character: start_character,
            },
            end: Position {
                line: end_line,
                character: end_character,
            },
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::{completions, diagnostics, document_symbols, hover, inlay_hints, signature_help};

    const SOURCE: &str = include_str!("browser/fixtures/browser-intelligence.orna");
    const STANDARD_SOURCE: &str = include_str!("analysis/fixtures/standard-math-import.orna");
    const RANKED_STANDARD_SOURCE: &str =
        include_str!("browser/fixtures/standard-completion-ranking.orna");
    const STANDARD_SIGNATURE_SOURCE: &str =
        include_str!("browser/fixtures/standard-signature-help.orna");
    const STANDARD_INLAY_SOURCE: &str = include_str!("browser/fixtures/standard-inlay-hints.orna");

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
            "pub fn clamp(value: Int, lower: Int, upper: Int): Int"
        );
        assert!(
            standard_signature["documentation"]
                .to_string()
                .contains("inclusive interval")
        );
        let parameters = standard_signature["parameters"]
            .as_array()
            .expect("parameter hints");
        assert_eq!(parameters.len(), 3);
        assert_eq!(parameters[0]["label"], "value: Int");
        assert_eq!(parameters[1]["label"], "lower: Int");
        assert_eq!(parameters[2]["label"], "upper: Int");
    }

    #[test]
    fn browser_standard_inlay_hints_cover_imported_and_qualified_calls() {
        let (end_line, end_character) =
            position(STANDARD_INLAY_SOURCE, STANDARD_INLAY_SOURCE.len());
        let hints: serde_json::Value = serde_json::from_str(&inlay_hints(
            STANDARD_INLAY_SOURCE.to_owned(),
            0,
            0,
            end_line,
            end_character,
        ))
        .expect("standard inlay hints JSON");
        let hints = hints.as_array().expect("inlay hint list");
        let labels = hints
            .iter()
            .map(|hint| hint["label"].as_str().expect("hint label"))
            .collect::<Vec<_>>();
        assert!(labels.contains(&": Int"), "{labels:?}");
        assert!(labels.contains(&"value: "), "{labels:?}");
        assert!(labels.contains(&"lower: "), "{labels:?}");
        assert!(labels.contains(&"upper: "), "{labels:?}");

        let qualified_start = STANDARD_INLAY_SOURCE
            .find("std.math.clamp(value, lower, upper)")
            .expect("qualified clamp call");
        let qualified_end = qualified_start + "std.math.clamp(value, lower, upper)".len();
        let (start_line, start_character) = position(STANDARD_INLAY_SOURCE, qualified_start);
        let (end_line, end_character) = position(STANDARD_INLAY_SOURCE, qualified_end);
        let ranged: serde_json::Value = serde_json::from_str(&inlay_hints(
            STANDARD_INLAY_SOURCE.to_owned(),
            start_line,
            start_character,
            end_line,
            end_character,
        ))
        .expect("qualified call inlay hints JSON");
        let ranged_labels = ranged
            .as_array()
            .expect("qualified call hints")
            .iter()
            .map(|hint| hint["label"].as_str().expect("hint label"))
            .collect::<Vec<_>>();
        assert_eq!(ranged_labels, ["value: ", "lower: ", "upper: "]);
    }

    #[test]
    fn browser_inlay_hint_documents_remain_independent_under_concurrent_requests() {
        let shifted_source = format!("\n\n{STANDARD_INLAY_SOURCE}");
        let (base_json, shifted_json) = std::thread::scope(|scope| {
            let base = scope.spawn(|| {
                let (end_line, end_character) =
                    position(STANDARD_INLAY_SOURCE, STANDARD_INLAY_SOURCE.len());
                inlay_hints(
                    STANDARD_INLAY_SOURCE.to_owned(),
                    0,
                    0,
                    end_line,
                    end_character,
                )
            });
            let shifted = scope.spawn(|| {
                let (end_line, end_character) = position(&shifted_source, shifted_source.len());
                inlay_hints(shifted_source.clone(), 0, 0, end_line, end_character)
            });
            (
                base.join().expect("join base inlay request"),
                shifted.join().expect("join shifted inlay request"),
            )
        });

        let base: serde_json::Value = serde_json::from_str(&base_json).expect("base hint JSON");
        let shifted: serde_json::Value =
            serde_json::from_str(&shifted_json).expect("shifted hint JSON");
        let decode = |response: &serde_json::Value| {
            response
                .as_array()
                .expect("inlay hint list")
                .iter()
                .map(|hint| {
                    (
                        hint["label"].as_str().expect("hint label").to_owned(),
                        hint["position"]["line"].as_u64().expect("hint line"),
                        hint["position"]["character"]
                            .as_u64()
                            .expect("hint character"),
                    )
                })
                .collect::<Vec<_>>()
        };
        let base = decode(&base);
        let shifted = decode(&shifted);
        assert_eq!(
            base.iter()
                .map(|(label, _, _)| label.as_str())
                .collect::<Vec<_>>(),
            shifted
                .iter()
                .map(|(label, _, _)| label.as_str())
                .collect::<Vec<_>>(),
            "parallel documents should produce the same hints"
        );
        assert_eq!(base.len(), 8, "standard fixture hint count: {base:?}");
        for (
            (base_label, base_line, base_character),
            (shifted_label, shifted_line, shifted_character),
        ) in base.iter().zip(&shifted)
        {
            assert_eq!(base_label, shifted_label);
            assert_eq!(
                *shifted_line,
                *base_line + 2,
                "line belongs to its source document"
            );
            assert_eq!(shifted_character, base_character);
        }
    }

    #[test]
    fn browser_document_symbols_expose_the_nested_outline_with_lsp_ranges() {
        const DOCUMENT_SYMBOLS_SOURCE: &str =
            include_str!("browser/fixtures/standard-document-symbols.orna");
        let symbols: serde_json::Value =
            serde_json::from_str(&document_symbols(DOCUMENT_SYMBOLS_SOURCE.to_owned()))
                .expect("document symbol JSON");
        let outcome = &symbols[0];
        assert_eq!(outcome["name"], "Outcome");
        assert_eq!(outcome["range"]["start"]["line"], 1, "{outcome:#}");
        let children: Vec<&str> = outcome["children"]
            .as_array()
            .expect("enum variants are nested children")
            .iter()
            .map(|child| child["name"].as_str().expect("child name"))
            .collect();
        assert_eq!(children, ["success", "failed"], "{outcome:#}");
    }
}
