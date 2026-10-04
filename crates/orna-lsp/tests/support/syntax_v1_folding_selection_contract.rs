//! Shared syntax-v1 editor contracts for folding and selection ranges.

use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Cursor positions reused by protocol, Vim, Neovim, and Emacs/Eglot probes.
pub fn request_data(source: &str) -> Value {
    let value_start = source.find("abs(value)").expect("nested value") + "abs(".len();
    let compass_end = source.find("\"🧭\"").expect("compass string") + "\"🧭\"".len();
    let comment_cursor = source
        .find("compass 🧭 remains")
        .expect("Unicode documentation comment")
        + "compass ".len();
    let marker_cursor = source
        .find("string content")
        .expect("comment-looking string content")
        + "string ".len();
    let blank_line = source
        .find("\n\npub fn compass")
        .expect("blank line before compass")
        + 1;
    json!({
        "selection_positions": [
            position_at(source, value_start + 2),
            position_at(source, compass_end - 1),
            position_at(source, comment_cursor),
            position_at(source, marker_cursor),
            position_at(source, blank_line),
        ]
    })
}

/// Checks syntax-driven folds, ordering and deduplication, plus syntax-parent
/// selection chains at nested expressions and UTF-16-sensitive positions.
pub fn assert_contract(source: &str, folds: &Value, selections: &Value, editor: &str) {
    assert_folding_contract(source, folds, editor);
    assert_selection_contract(source, selections, editor);
}

fn assert_folding_contract(source: &str, response: &Value, editor: &str) {
    let folds = response
        .as_array()
        .unwrap_or_else(|| panic!("{editor} folding result is not an array: {response}"));
    assert!(!folds.is_empty(), "{editor} returned no folding ranges");
    let mut previous = None;
    let mut unique = BTreeSet::new();
    for fold in folds {
        let key = (
            fold["startLine"].as_u64().unwrap(),
            fold["startCharacter"].as_u64().unwrap(),
            fold["endLine"].as_u64().unwrap(),
            fold["endCharacter"].as_u64().unwrap(),
        );
        assert!(
            key.0 < key.2,
            "{editor} returned a single-line fold: {fold}"
        );
        assert!(
            previous.is_none_or(|previous| previous <= key),
            "{editor} folding ranges are unsorted: {folds:?}"
        );
        assert!(
            unique.insert(key),
            "{editor} returned duplicate folding coordinates: {fold}"
        );
        previous = Some(key);
    }

    let comment_start = source.find("/* range docs").expect("doc comment start");
    let comment_end = source.find("*/").expect("doc comment end") + 2;
    let comment_fold = folds
        .iter()
        .find(|fold| fold["kind"] == "comment")
        .expect("multi-line comment folding range");
    assert_eq!(
        comment_fold,
        &folding_range_at(source, comment_start, comment_end, "comment"),
        "{editor} doc comment fold differs"
    );

    let import_start = source.find("use std.math.{abs}").expect("import start");
    let import_end =
        source.find("use std.math.{min}").expect("second import") + "use std.math.{min};".len();
    let import_fold = folds
        .iter()
        .find(|fold| fold["kind"] == "imports")
        .expect("contiguous import folding range");
    assert_eq!(
        import_fold,
        &folding_range_at(source, import_start, import_end, "imports"),
        "{editor} contiguous import fold differs"
    );

    let line_comment_start = source.find("// fold together").expect("line comment start");
    let line_comment_end = source
        .find("// contiguous comments")
        .expect("line comment end")
        + "// contiguous comments".len();
    let line_comment_fold = folds
        .iter()
        .find(|fold| {
            fold["kind"] == "comment"
                && fold["startLine"] == position_at(source, line_comment_start)["line"]
        })
        .expect("contiguous line-comment folding range");
    assert_eq!(
        line_comment_fold,
        &folding_range_at(source, line_comment_start, line_comment_end, "comment"),
        "{editor} contiguous line-comment fold differs"
    );

    let control_start = source.find("if total > 0").expect("control body start");
    let control_end = source
        .find("\n    }\n}\n\npub enum")
        .expect("control body end")
        + 6;
    assert!(
        folds.iter().any(|fold| {
            fold["startLine"] == position_at(source, control_start)["line"]
                && fold["endLine"] == position_at(source, control_end)["line"]
        }),
        "{editor} control-body folding range missing: {folds:?}"
    );

    let outer_start = source.find("pub fn outer").expect("outer function start");
    let outer_end = source
        .find("\n}\n\npub enum Outcome")
        .expect("outer function end")
        + 2;
    assert!(
        folds.iter().any(|fold| {
            fold["startLine"] == position_at(source, outer_start)["line"]
                && fold["startCharacter"] == position_at(source, outer_start)["character"]
                && fold["endLine"] == position_at(source, outer_end)["line"]
        }),
        "{editor} top-level function folding range missing: {folds:?}"
    );

    let enum_start = source.find("pub enum Outcome").expect("enum start");
    let enum_end = source
        .find("failed { reason: Str },")
        .expect("enum final variant")
        + "failed { reason: Str },\n}".len();
    assert!(
        folds.iter().any(|fold| {
            fold["startLine"] == position_at(source, enum_start)["line"]
                && fold["startCharacter"] == position_at(source, enum_start)["character"]
                && fold["endLine"] == position_at(source, enum_end)["line"]
        }),
        "{editor} enum folding range missing: {folds:?}"
    );
}

fn assert_selection_contract(source: &str, response: &Value, editor: &str) {
    let selections = response
        .as_array()
        .unwrap_or_else(|| panic!("{editor} selection result is not an array: {response}"));
    assert_eq!(selections.len(), 5, "{editor} selection results differ");
    let value_start = source.find("abs(value)").unwrap() + "abs(".len();
    let value_end = value_start + "value".len();
    let value_chain = selection_chain(&selections[0]);
    assert_eq!(
        value_chain[0]["range"],
        range_at(source, value_start, value_end),
        "{editor} selected the wrong nested identifier"
    );
    let call_start = source.find("abs(value)").unwrap();
    let call_end = call_start + "abs(value)".len();
    assert_eq!(
        value_chain[1]["range"],
        range_at(source, call_start, call_end),
        "{editor} selection omitted the enclosing call"
    );
    let statement_start = source.find("let total =").unwrap();
    let statement_end = statement_start + "let total = abs(value)".len();
    assert_eq!(
        value_chain[2]["range"],
        range_at(source, statement_start, statement_end),
        "{editor} selection omitted the enclosing statement"
    );
    assert!(
        value_chain.len() >= 5,
        "{editor} selection ancestry stopped before the document: {value_chain:?}"
    );
    assert_eq!(
        value_chain.last().unwrap()["range"],
        range_at(source, 0, source.len()),
        "{editor} selection ancestry has the wrong document range"
    );

    let compass_start = source.find("\"🧭\"").unwrap();
    let compass_end = compass_start + "\"🧭\"".len();
    let compass_chain = selection_chain(&selections[1]);
    assert_eq!(
        compass_chain[0]["range"],
        range_at(source, compass_start, compass_end),
        "{editor} UTF-16 cursor after supplementary character selected the wrong token"
    );

    let comment_start = source.find("/* range docs").unwrap();
    let comment_end = source.find("*/").unwrap() + 2;
    assert_eq!(
        selection_chain(&selections[2])[0]["range"],
        range_at(source, comment_start, comment_end),
        "{editor} Unicode documentation comment selection differs"
    );

    let marker_start = source
        .find("\"/* string content, never a comment */\"")
        .unwrap();
    let marker_end = marker_start + "\"/* string content, never a comment */\"".len();
    assert_eq!(
        selection_chain(&selections[3])[0]["range"],
        range_at(source, marker_start, marker_end),
        "{editor} comment-looking string was not selected as a string"
    );

    let blank_line = source.find("\n\npub fn compass").unwrap() + 1;
    let blank_chain = selection_chain(&selections[4]);
    assert_eq!(
        blank_chain[0]["range"],
        range_at(source, blank_line, blank_line),
        "{editor} blank-line selection differs"
    );
    assert_eq!(
        blank_chain[1]["range"],
        range_at(source, 0, source.len()),
        "{editor} blank-line selection did not include the document"
    );
}

fn selection_chain(selection: &Value) -> Vec<Value> {
    let mut chain = Vec::new();
    let mut current = selection;
    loop {
        chain.push(current.clone());
        let Some(parent) = current.get("parent") else {
            break;
        };
        current = parent;
    }
    chain
}

fn folding_range_at(source: &str, start: usize, end: usize, kind: &str) -> Value {
    let start = position_at(source, start);
    let end = position_at(source, end);
    json!({
        "startLine": start["line"],
        "startCharacter": start["character"],
        "endLine": end["line"],
        "endCharacter": end["character"],
        "kind": kind,
    })
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
