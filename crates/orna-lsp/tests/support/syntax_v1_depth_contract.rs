//! Shared editor-attachment proofs for syntax-v1 semantic and inlay depth.

use serde_json::{Value, json};

type SemanticToken = (usize, usize, String, String);
type InlayHint = (usize, usize, String, u64, Option<bool>, Option<bool>);

const TOKEN_TYPES: &[&str] = &[
    "keyword",
    "variable",
    "number",
    "string",
    "comment",
    "operator",
    "type",
    "enum",
    "interface",
    "function",
    "parameter",
];

/// Ranges shared by the real editor probes. The hint fixture is deliberately
/// ASCII so byte offsets and LSP UTF-16 characters are identical.
pub fn request_ranges(source: &str) -> Value {
    assert!(source.is_ascii(), "syntax-v1 depth fixture must be ASCII");
    let function_start = source.find("pub fn add(").expect("add declaration");
    let function_end = source[function_start..]
        .find('\n')
        .map(|offset| function_start + offset + 1)
        .unwrap_or(source.len());
    let call_start = source
        .find("add(inferred, annotated)")
        .expect("inferred call");
    let call_end = source[call_start..]
        .find(';')
        .map(|offset| call_start + offset + 1)
        .expect("inferred call terminator");
    let inferred_start = source.find("let inferred").expect("inferred local");
    let inferred_end = source[inferred_start..]
        .find(';')
        .map(|offset| inferred_start + offset + 1)
        .expect("inferred local terminator");
    let annotated_start = source.find("let annotated").expect("annotated local");
    let annotated_end = source[annotated_start..]
        .find(';')
        .map(|offset| annotated_start + offset + 1)
        .expect("annotated local terminator");
    let shadow_start = source
        .find("add(1)")
        .filter(|start| *start > source.find("pub fn shadowed").expect("shadowed function"))
        .expect("shadowed local call");
    let shadow_end = shadow_start + "add(1)".len();

    json!({
        "semantic": range(source, function_start, function_end),
        "hints_full": range(source, 0, source.len()),
        "hints_call": range(source, call_start, call_end),
        "hints_inferred": range(source, inferred_start, inferred_end),
        "hints_annotated": range(source, annotated_start, annotated_end),
        "hints_shadowed": range(source, shadow_start, shadow_end),
    })
}

/// Checks semantic class refinement and that ranged responses exactly match
/// the corresponding portion of the full response.
pub fn assert_semantic_depth_contract(source: &str, full: &Value, ranged: &Value, editor: &str) {
    assert!(source.is_ascii(), "{editor} depth fixture must be ASCII");
    let full_tokens = decode_semantic_tokens(source, full, editor);
    for (text, class) in [
        ("User", "type"),
        ("add", "function"),
        ("left", "parameter"),
        ("right", "parameter"),
        ("inferred", "variable"),
        ("annotated", "variable"),
        ("add", "parameter"),
    ] {
        assert!(
            full_tokens
                .iter()
                .any(|(_, _, token, kind)| token == text && kind == class),
            "{editor} semantic tokens did not classify {text:?} as {class}: {full_tokens:?}"
        );
    }
    assert!(
        full_tokens
            .iter()
            .filter(|(_, _, token, kind)| token == "add" && kind == "function")
            .count()
            >= 2,
        "{editor} semantic tokens did not classify both the add declaration and call as functions: {full_tokens:?}"
    );

    let range = request_ranges(source)["semantic"].clone();
    let start_line = range["start"]["line"].as_u64().unwrap() as usize;
    let start_character = range["start"]["character"].as_u64().unwrap() as usize;
    let end_line = range["end"]["line"].as_u64().unwrap() as usize;
    let end_character = range["end"]["character"].as_u64().unwrap() as usize;
    let expected = full_tokens
        .iter()
        .filter(|(line, character, _, _)| {
            (*line, *character) >= (start_line, start_character)
                && (*line, *character) < (end_line, end_character)
        })
        .cloned()
        .collect::<Vec<_>>();
    let actual = decode_semantic_tokens(source, ranged, editor);
    assert!(
        !expected.is_empty(),
        "{editor} semantic range had no fixture tokens"
    );
    assert_eq!(
        actual, expected,
        "{editor} ranged semantic tokens differ from the corresponding full-response slice"
    );
}

/// Checks exact type/parameter hint labels, classes, positions, and range
/// filtering on the same attached document.
pub fn assert_inlay_hint_depth_contract(
    source: &str,
    full: &Value,
    call: &Value,
    inferred: &Value,
    annotated: &Value,
    shadowed: &Value,
    editor: &str,
) {
    assert!(source.is_ascii(), "{editor} depth fixture must be ASCII");
    let inferred_name = source.find("let inferred").unwrap() + "let ".len();
    let inferred_hint = inferred_name + "inferred".len();
    let call_start = source.find("add(inferred, annotated)").unwrap();
    let left_argument = call_start + "add(".len();
    let right_argument = source[call_start..].find("annotated").unwrap() + call_start;
    let expected = vec![
        hint_at(source, inferred_hint, ": Int", 1, Some(true), None),
        hint_at(source, left_argument, "left: ", 2, None, Some(true)),
        hint_at(source, right_argument, "right: ", 2, None, Some(true)),
    ];
    let actual = decode_inlay_hints(full, editor);
    assert_eq!(actual, expected, "{editor} full inlay-hint depth differs");

    assert_eq!(
        decode_inlay_hints(call, editor),
        expected[1..].to_vec(),
        "{editor} call-range parameter hints differ"
    );
    assert_eq!(
        decode_inlay_hints(inferred, editor),
        expected[..1].to_vec(),
        "{editor} inferred-local range type hint differs"
    );
    assert!(
        decode_inlay_hints(annotated, editor).is_empty(),
        "{editor} added a redundant type hint to an annotated local"
    );
    assert!(
        decode_inlay_hints(shadowed, editor).is_empty(),
        "{editor} treated a shadowing local parameter as a function call"
    );
}

fn decode_semantic_tokens(source: &str, response: &Value, editor: &str) -> Vec<SemanticToken> {
    let data = response["data"]
        .as_array()
        .unwrap_or_else(|| panic!("{editor} semantic response has no data array: {response}"));
    assert_eq!(
        data.len() % 5,
        0,
        "{editor} semantic-token data is not five integers per token"
    );
    let lines = source.lines().collect::<Vec<_>>();
    let mut line = 0usize;
    let mut character = 0usize;
    let mut tokens = Vec::with_capacity(data.len() / 5);
    for token in data.chunks_exact(5) {
        let delta_line = token[0].as_u64().unwrap() as usize;
        let delta_start = token[1].as_u64().unwrap() as usize;
        if delta_line == 0 {
            character += delta_start;
        } else {
            line += delta_line;
            character = delta_start;
        }
        let length = token[2].as_u64().unwrap() as usize;
        let token_type = token[3].as_u64().unwrap() as usize;
        let token_type = TOKEN_TYPES
            .get(token_type)
            .unwrap_or_else(|| panic!("{editor} semantic token type {token_type} is unknown"));
        let text = lines
            .get(line)
            .and_then(|source_line| source_line.get(character..character + length))
            .unwrap_or_else(|| {
                panic!("{editor} semantic token is outside source at {line}:{character}+{length}")
            });
        tokens.push((line, character, text.to_owned(), (*token_type).to_owned()));
    }
    tokens
}

fn decode_inlay_hints(response: &Value, editor: &str) -> Vec<InlayHint> {
    response
        .as_array()
        .unwrap_or_else(|| panic!("{editor} inlay response is not an array: {response}"))
        .iter()
        .map(|hint| {
            (
                hint["position"]["line"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("{editor} inlay hint has no line: {hint}"))
                    as usize,
                hint["position"]["character"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("{editor} inlay hint has no character: {hint}"))
                    as usize,
                hint["label"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{editor} inlay hint label is not a string: {hint}"))
                    .to_owned(),
                hint["kind"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("{editor} inlay hint has no kind: {hint}")),
                hint["paddingLeft"].as_bool(),
                hint["paddingRight"].as_bool(),
            )
        })
        .collect()
}

fn hint_at(
    source: &str,
    offset: usize,
    label: &str,
    kind: u64,
    padding_left: Option<bool>,
    padding_right: Option<bool>,
) -> InlayHint {
    let position = position_at(source, offset);
    (
        position["line"].as_u64().unwrap() as usize,
        position["character"].as_u64().unwrap() as usize,
        label.to_owned(),
        kind,
        padding_left,
        padding_right,
    )
}

fn range(source: &str, start: usize, end: usize) -> Value {
    json!({ "start": position_at(source, start), "end": position_at(source, end) })
}

fn position_at(source: &str, byte: usize) -> Value {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let character = prefix
        .rsplit_once('\n')
        .map_or(prefix.len(), |(_, tail)| tail.len());
    json!({ "line": line, "character": character })
}
