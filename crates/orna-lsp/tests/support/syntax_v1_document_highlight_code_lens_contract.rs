//! Shared syntax-v1 editor contracts for document highlights and code lenses.

use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Cursor used to request variable highlights from each attached editor.
pub fn document_highlight_position(source: &str) -> Value {
    let start = source
        .find("count =")
        .expect("document highlight fixture assignment")
        + 1;
    position_at(source, start)
}

/// Checks exact variable ranges and read/write classifications.
pub fn assert_document_highlights_contract(source: &str, response: &Value, editor: &str) {
    let highlights = response.as_array().unwrap_or_else(|| {
        panic!("{editor} document-highlight response is not an array: {response}")
    });
    let expected = source
        .match_indices("count")
        .map(|(start, word)| range_at(source, start, start + word.len()))
        .collect::<Vec<_>>();
    assert_eq!(expected.len(), 5, "fixture count references changed");
    assert_eq!(
        highlights.len(),
        expected.len(),
        "{editor} highlights differ"
    );
    assert_eq!(
        highlights
            .iter()
            .map(|highlight| highlight["range"].clone())
            .collect::<Vec<_>>(),
        expected,
        "{editor} highlight ranges differ"
    );
    assert_eq!(
        highlights
            .iter()
            .map(|highlight| highlight["kind"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [3, 3, 2, 2, 2],
        "{editor} read/write classifications differ"
    );
}

/// Checks workspace call counts, exact declaration ranges, commands, and
/// reference locations returned through an attached editor client.
pub fn assert_code_lens_contract(
    provider_source: &str,
    caller_source: &str,
    provider_uri: &str,
    caller_uri: &str,
    provider_response: &Value,
    caller_response: &Value,
    editor: &str,
) {
    let provider_lenses = provider_response.as_array().unwrap_or_else(|| {
        panic!("{editor} provider code-lens response is not an array: {provider_response}")
    });
    let caller_lenses = caller_response.as_array().unwrap_or_else(|| {
        panic!("{editor} caller code-lens response is not an array: {caller_response}")
    });
    assert_eq!(provider_lenses.len(), 7, "{editor} provider lenses differ");
    assert_eq!(caller_lenses.len(), 4, "{editor} caller lenses differ");

    for (name, title, references) in [
        (
            "seed",
            "3 incoming calls",
            locations(provider_uri, provider_source, "seed(value)"),
        ),
        (
            "mid",
            "2 incoming calls",
            vec![
                location(provider_uri, provider_source, "mid(seed"),
                location(caller_uri, caller_source, "mid(root"),
            ],
        ),
        (
            "root",
            "2 incoming calls",
            locations(caller_uri, caller_source, "root(value)"),
        ),
        ("shadowed", "0 incoming calls", Vec::new()),
        ("isolated", "0 incoming calls", Vec::new()),
        ("unresolved", "0 incoming calls", Vec::new()),
        ("collide", "0 incoming calls", Vec::new()),
    ] {
        let lens = find_lens(provider_lenses, provider_source, name, editor);
        let selection_start = after(provider_source, &format!("pub fn {name}"), "pub fn ");
        assert_eq!(
            lens["range"],
            range_at(
                provider_source,
                selection_start,
                selection_start + name.len()
            ),
            "{editor} {name} code-lens range differs"
        );
        assert_eq!(
            lens["command"]["title"], title,
            "{editor} {name} code-lens title differs"
        );
        assert_eq!(
            lens["command"]["command"], "editor.action.showReferences",
            "{editor} {name} code-lens command differs"
        );
        assert_eq!(lens["command"]["arguments"][0], provider_uri);
        assert_eq!(
            lens["command"]["arguments"][1],
            position_at(provider_source, selection_start),
            "{editor} {name} code-lens position differs"
        );
        assert_location_set(&lens["command"]["arguments"][2], references, editor, name);
    }

    for name in ["remote_root", "remote_mid", "collide", "ambiguous_call"] {
        let lens = find_lens(caller_lenses, caller_source, name, editor);
        let selection_start = after(caller_source, &format!("pub fn {name}"), "pub fn ");
        assert_eq!(
            lens["range"],
            range_at(caller_source, selection_start, selection_start + name.len()),
            "{editor} caller {name} code-lens range differs"
        );
        assert_eq!(lens["command"]["title"], "0 incoming calls");
        assert_eq!(lens["command"]["command"], "editor.action.showReferences");
        assert_eq!(lens["command"]["arguments"][0], caller_uri);
        assert_eq!(
            lens["command"]["arguments"][1],
            position_at(caller_source, selection_start)
        );
        assert_location_set(&lens["command"]["arguments"][2], Vec::new(), editor, name);
    }
}

fn find_lens<'a>(lenses: &'a [Value], source: &str, name: &str, editor: &str) -> &'a Value {
    let selection_start = after(source, &format!("pub fn {name}"), "pub fn ");
    let expected = range_at(source, selection_start, selection_start + name.len());
    lenses
        .iter()
        .find(|lens| lens["range"] == expected)
        .unwrap_or_else(|| panic!("{editor} omitted {name} code lens: {lenses:?}"))
}

fn assert_location_set(response: &Value, expected: Vec<Value>, editor: &str, name: &str) {
    let locations = response.as_array().unwrap_or_else(|| {
        panic!("{editor} {name} code-lens references are not an array: {response}")
    });
    assert_eq!(
        location_set(locations),
        location_set(&expected),
        "{editor} {name} code-lens reference locations differ"
    );
}

fn location_set(locations: &[Value]) -> BTreeSet<(String, u64, u64, u64, u64)> {
    locations
        .iter()
        .map(|location| {
            (
                location["uri"].as_str().unwrap().to_owned(),
                location["range"]["start"]["line"].as_u64().unwrap(),
                location["range"]["start"]["character"].as_u64().unwrap(),
                location["range"]["end"]["line"].as_u64().unwrap(),
                location["range"]["end"]["character"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn locations(uri: &str, source: &str, needle: &str) -> Vec<Value> {
    source
        .match_indices(needle)
        .map(|(start, _)| location_at(uri, source, start, needle.split('(').next().unwrap().len()))
        .collect()
}

fn location(uri: &str, source: &str, needle: &str) -> Value {
    let start = source.find(needle).expect("workspace call reference");
    location_at(uri, source, start, needle.split('(').next().unwrap().len())
}

fn location_at(uri: &str, source: &str, start: usize, length: usize) -> Value {
    json!({
        "uri": uri,
        "range": range_at(source, start, start + length),
    })
}

fn range_at(source: &str, start: usize, end: usize) -> Value {
    json!({
        "start": position_at(source, start),
        "end": position_at(source, end),
    })
}

fn position_at(source: &str, byte: usize) -> Value {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
    let character = source[line_start..byte].encode_utf16().count();
    json!({ "line": line, "character": character })
}

fn after(source: &str, needle: &str, offset: &str) -> usize {
    source.find(needle).expect("fixture declaration") + offset.len()
}
