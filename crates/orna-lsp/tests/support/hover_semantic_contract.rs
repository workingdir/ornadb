use std::collections::BTreeSet;

use serde_json::Value;

const SEMANTIC_TOKEN_TYPES: &[&str] = &[
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

pub fn assert_hover_contract(hover: &Value, editor: &str) {
    let contents = &hover["contents"];
    assert_eq!(
        contents["kind"], "markdown",
        "{editor} hover did not return Markdown content"
    );
    let value = contents["value"]
        .as_str()
        .unwrap_or_else(|| panic!("{editor} hover result has no Markdown value"));
    assert!(
        value.contains("pub fn add(left: Int, right: Int): Int"),
        "{editor} hover omitted the exported add signature: {value}"
    );
    assert!(
        value.contains("Add two integer values."),
        "{editor} hover omitted fixture documentation: {value}"
    );
}

pub fn assert_semantic_token_contract(source: &str, response: &Value, editor: &str) {
    assert!(
        source.is_ascii(),
        "semantic fixture must use ASCII token offsets"
    );
    let data = response["data"]
        .as_array()
        .unwrap_or_else(|| panic!("{editor} semantic-token result has no data array"));
    assert_eq!(
        data.len() % 5,
        0,
        "{editor} semantic-token data is not five integers per token"
    );
    let lines = source.lines().collect::<Vec<_>>();
    let mut line = 0usize;
    let mut character = 0usize;
    let mut tokens = Vec::with_capacity(data.len() / 5);
    for encoded in data.chunks_exact(5) {
        let delta_line = encoded[0].as_u64().unwrap() as usize;
        let delta_start = encoded[1].as_u64().unwrap() as usize;
        if delta_line == 0 {
            character += delta_start;
        } else {
            line += delta_line;
            character = delta_start;
        }
        let length = encoded[2].as_u64().unwrap() as usize;
        let token_type = encoded[3].as_u64().unwrap() as usize;
        let token_type = SEMANTIC_TOKEN_TYPES.get(token_type).unwrap_or_else(|| {
            panic!("{editor} semantic token type index {token_type} is unknown")
        });
        let token_line = lines.get(line).unwrap_or_else(|| {
            panic!("{editor} semantic token is outside the source at line {line}")
        });
        let token_text = token_line
            .get(character..character + length)
            .unwrap_or_else(|| {
                panic!("{editor} semantic token range is invalid at {line}:{character}+{length}")
            });
        tokens.push((token_text, *token_type));
    }

    for (text, expected_type) in [
        ("let", "keyword"),
        ("total", "variable"),
        ("12", "number"),
        ("+", "operator"),
    ] {
        assert!(
            tokens
                .iter()
                .any(|(token_text, token_type)| *token_text == text && *token_type == expected_type),
            "{editor} semantic tokens did not classify {text:?} as {expected_type}: {tokens:?}"
        );
    }
    assert!(
        tokens
            .iter()
            .any(|(token_text, token_type)| token_text.contains("hello") && *token_type == "string"),
        "{editor} semantic tokens omitted the string class: {tokens:?}"
    );
    assert!(
        tokens
            .iter()
            .any(|(token_text, token_type)| token_text.contains("comment")
                && *token_type == "comment"),
        "{editor} semantic tokens omitted the comment class: {tokens:?}"
    );

    let actual_types = tokens
        .iter()
        .map(|(_, token_type)| (*token_type).to_owned())
        .collect::<BTreeSet<_>>();
    let expected_types = [
        "comment", "keyword", "number", "operator", "string", "variable",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert!(
        expected_types.is_subset(&actual_types),
        "{editor} semantic token classes differ: expected {expected_types:?}, got {actual_types:?}"
    );
}

pub fn assert_semantic_token_legend(legend: &Value, editor: &str) {
    let actual = legend
        .as_array()
        .unwrap_or_else(|| panic!("{editor} did not expose a semantic token legend"))
        .iter()
        .map(|token_type| {
            token_type
                .as_str()
                .unwrap_or_else(|| panic!("{editor} semantic token legend has a non-string entry"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        SEMANTIC_TOKEN_TYPES.to_vec(),
        "{editor} semantic token legend differs from the syntax-v1 contract"
    );
}

type SymbolLocation = (String, usize, usize, usize, usize);

pub fn assert_references_contract(
    sources: &[(&str, &str)],
    response: &Value,
    include_declaration: bool,
    editor: &str,
) {
    let locations = response
        .as_array()
        .unwrap_or_else(|| panic!("{editor} references result is not an array: {response}"));
    let expected = expected_symbol_locations(sources, include_declaration, editor);
    let actual = locations
        .iter()
        .map(|location| location_key(location, editor))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual.len(),
        locations.len(),
        "{editor} returned duplicate references: {response}"
    );
    assert_eq!(
        actual, expected,
        "{editor} references differ from syntax-v1 declaration/call locations"
    );
}

pub fn assert_rename_contract(
    sources: &[(&str, &str)],
    response: &Value,
    new_name: &str,
    editor: &str,
) {
    let changes = response["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("{editor} rename result has no changes map: {response}"));
    let expected = expected_symbol_locations(sources, true, editor);
    let expected_uris = expected
        .iter()
        .map(|(uri, _, _, _, _)| uri.clone())
        .collect::<BTreeSet<_>>();
    let actual_uris = changes.keys().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        actual_uris, expected_uris,
        "{editor} rename changed a different set of documents"
    );
    let mut actual = BTreeSet::new();
    for (uri, edits) in changes {
        let edits = edits
            .as_array()
            .unwrap_or_else(|| panic!("{editor} rename edits for {uri} are not an array"));
        for edit in edits {
            assert_eq!(
                edit["newText"], new_name,
                "{editor} rename used unexpected replacement text: {edit}"
            );
            let key = location_key_with_uri(uri, edit, editor);
            assert!(
                actual.insert(key),
                "{editor} rename returned a duplicate edit: {edit}"
            );
        }
    }
    assert_eq!(
        actual, expected,
        "{editor} rename differs from syntax-v1 declaration/call locations"
    );
}

fn expected_symbol_locations(
    sources: &[(&str, &str)],
    include_declaration: bool,
    editor: &str,
) -> BTreeSet<SymbolLocation> {
    let mut locations = BTreeSet::new();
    for (uri, source) in sources {
        assert!(source.is_ascii(), "{editor} symbol fixture must be ASCII");
        for (start, _) in source.match_indices("add(") {
            let end = start + "add".len();
            let before = source[..start].chars().next_back();
            let after = source[end..].chars().next();
            if before.is_some_and(is_identifier_char) || after != Some('(') {
                continue;
            }
            let is_declaration = source[..start].ends_with("pub fn ");
            if is_declaration && !include_declaration {
                continue;
            }
            let (line, character) = position_at(source, start);
            let (end_line, end_character) = position_at(source, end);
            locations.insert(((*uri).to_owned(), line, character, end_line, end_character));
        }
    }
    assert!(
        !locations.is_empty(),
        "{editor} symbol contract fixture has no add declaration/calls"
    );
    locations
}

fn location_key(location: &Value, editor: &str) -> SymbolLocation {
    let uri = location["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("{editor} location has no URI: {location}"));
    location_key_with_uri(uri, location, editor)
}

fn location_key_with_uri(uri: &str, location: &Value, editor: &str) -> SymbolLocation {
    let range = &location["range"];
    let start = &range["start"];
    let end = &range["end"];
    (
        uri.to_owned(),
        start["line"]
            .as_u64()
            .unwrap_or_else(|| panic!("{editor} range start has no line: {location}"))
            as usize,
        start["character"]
            .as_u64()
            .unwrap_or_else(|| panic!("{editor} range start has no character: {location}"))
            as usize,
        end["line"]
            .as_u64()
            .unwrap_or_else(|| panic!("{editor} range end has no line: {location}"))
            as usize,
        end["character"]
            .as_u64()
            .unwrap_or_else(|| panic!("{editor} range end has no character: {location}"))
            as usize,
    )
}

fn position_at(source: &str, byte: usize) -> (usize, usize) {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let character = prefix
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .encode_utf16()
        .count();
    (line, character)
}

fn is_identifier_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}
