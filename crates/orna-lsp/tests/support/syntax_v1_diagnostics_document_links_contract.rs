//! Shared syntax-v1 editor contracts for diagnostics and document links.

use serde_json::{Value, json};

/// Checks a publishDiagnostics envelope and its parser diagnostic payload.
#[allow(dead_code)]
pub fn assert_published_diagnostics(
    source: &str,
    uri: &str,
    version: u64,
    response: &Value,
    editor: &str,
) {
    assert_eq!(response["uri"], uri, "{editor} diagnostic URI differs");
    assert_eq!(
        response["version"].as_u64(),
        Some(version),
        "{editor} diagnostic version differs: {response}"
    );

    assert_diagnostics(source, &response["diagnostics"], editor);
}

/// Checks the parser diagnostic fields and their mapped UTF-16 source range.
pub fn assert_diagnostics(source: &str, response: &Value, editor: &str) {
    let expected = orna_syntax_v1::parse_module(source);
    let diagnostics = response
        .as_array()
        .unwrap_or_else(|| panic!("{editor} diagnostics are not an array: {response}"));
    assert_eq!(
        diagnostics.len(),
        expected.diagnostics.len(),
        "{editor} diagnostic count differs: {response}"
    );

    for (actual, expected) in diagnostics.iter().zip(expected.diagnostics) {
        assert_eq!(
            actual["source"], "orna-syntax-v1",
            "{editor} diagnostic source"
        );
        assert_eq!(actual["severity"], 1, "{editor} diagnostic severity");
        assert_eq!(
            actual["code"], expected.code,
            "{editor} diagnostic code differs"
        );
        assert_eq!(
            actual["range"],
            range_at(source, expected.span.start, expected.span.end),
            "{editor} diagnostic range differs"
        );
        assert_eq!(
            actual["data"]["title"], expected.title,
            "{editor} diagnostic title differs"
        );
        assert_eq!(
            actual["data"]["help"],
            json!(&expected.help),
            "{editor} diagnostic help differs"
        );
        assert_eq!(
            actual["data"]["notes"],
            json!(&expected.notes),
            "{editor} diagnostic notes differ"
        );
        assert_eq!(actual["message"], diagnostic_message(&expected));
    }
}

/// Checks that a corrected document publishes an empty versioned diagnostic set.
#[allow(dead_code)]
pub fn assert_diagnostics_cleared(uri: &str, version: u64, response: &Value, editor: &str) {
    assert_eq!(
        response["uri"], uri,
        "{editor} cleared diagnostic URI differs"
    );
    assert_eq!(
        response["version"].as_u64(),
        Some(version),
        "{editor} cleared diagnostic version differs: {response}"
    );
    assert_diagnostics_cleared_items(&response["diagnostics"], editor);
}

/// Checks that a pull-diagnostic result has no items after a valid edit.
pub fn assert_diagnostics_cleared_items(response: &Value, editor: &str) {
    assert_eq!(
        response,
        &json!([]),
        "{editor} retained diagnostics after a valid edit"
    );
}

/// Checks the exact import span and uniquely resolved module target.
pub fn assert_document_link(source: &str, response: &Value, target_uri: &str, editor: &str) {
    let links = response
        .as_array()
        .unwrap_or_else(|| panic!("{editor} document links are not an array: {response}"));
    assert_eq!(
        links.len(),
        1,
        "{editor} resolved link count differs: {links:?}"
    );
    let path_start = source
        .find("library.math")
        .expect("document-link fixture import path");
    assert_eq!(
        links[0]["range"],
        range_at(source, path_start, path_start + "library.math".len()),
        "{editor} document-link import range differs"
    );
    assert_eq!(
        links[0]["target"], target_uri,
        "{editor} document-link target differs"
    );
    assert_eq!(
        links[0]["tooltip"], "Open module `library.math`",
        "{editor} document-link tooltip differs"
    );
}

/// Checks fail-closed document-link behavior when no unique module is open.
pub fn assert_no_document_links(response: &Value, editor: &str) {
    assert_eq!(
        response,
        &json!([]),
        "{editor} guessed or retained a document-link target: {response}"
    );
}

fn diagnostic_message(diagnostic: &orna_syntax_v1::SyntaxDiagnostic) -> String {
    let mut value = if !diagnostic.title.trim().is_empty() && diagnostic.title != diagnostic.message
    {
        format!("{}: {}", diagnostic.title, diagnostic.message)
    } else {
        diagnostic.message.clone()
    };
    for (heading, details) in [("Help", &diagnostic.help), ("Note", &diagnostic.notes)] {
        for detail in details.iter().filter(|detail| !detail.trim().is_empty()) {
            value.push_str("\n\n");
            value.push_str(heading);
            value.push_str(": ");
            value.push_str(detail.trim());
        }
    }
    value
}

fn range_at(source: &str, start: usize, end: usize) -> Value {
    json!({"start": position_at(source, start), "end": position_at(source, end)})
}

fn position_at(source: &str, byte: usize) -> Value {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let tail = prefix.rsplit_once('\n').map_or(prefix, |(_, tail)| tail);
    json!({"line": line, "character": tail.encode_utf16().count()})
}
