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
