//! Shared syntax-v1 editor contracts for code actions and signature help.

use serde_json::{Value, json};

/// Positions and ranges used by real editor clients and the protocol proof.
pub fn request_data(signature_source: &str, action_source: &str) -> Value {
    assert!(signature_source.is_ascii());
    assert!(action_source.is_ascii());
    let nested_tuple = signature_source.find("(1, 2)").expect("nested tuple") + "(1, ".len();
    let named_argument = signature_source.find("last: 3").expect("named argument") + "last: ".len();
    let nested_named_argument = signature_source
        .find("extra: value")
        .expect("nested named argument")
        + "extra: ".len();
    let shadowed_call = signature_source.find("add(1)").expect("shadowed call") + 1;
    json!({
        "signature_nested_tuple": position_at(signature_source, nested_tuple),
        "signature_named_argument": position_at(signature_source, named_argument),
        "signature_nested_named_argument": position_at(signature_source, nested_named_argument),
        "signature_shadowed_call": position_at(signature_source, shadowed_call),
        "code_action_full_range": range(action_source, 0, action_source.len()),
        "code_action_outside_range": range(action_source, 0, 1),
    })
}

/// Proves active-parameter resolution for nested calls and reordered named
/// arguments, while rejecting a call shadowed by a local parameter.
pub fn assert_signature_help_contract(
    wrap_nested: &Value,
    wrap_named: &Value,
    add_nested_named: &Value,
    shadowed: &Value,
    editor: &str,
) {
    assert_signature(
        wrap_nested,
        "wrap",
        0,
        &["pair: (Int, Int)", "middle: Int", "last: Int"],
        editor,
    );
    assert_signature(
        wrap_named,
        "wrap",
        2,
        &["pair: (Int, Int)", "middle: Int", "last: Int"],
        editor,
    );
    assert_signature(
        add_nested_named,
        "add",
        2,
        &["left: Int", "right: Int", "extra: Int"],
        editor,
    );
    assert!(
        shadowed.is_null(),
        "{editor} signature help resolved a call shadowed by a local parameter: {shadowed}"
    );
}

/// Proves the quickfix carries the exact insertion edit, repairs the fixture,
/// and respects both requested kind and range filters.
pub fn assert_code_action_contract(
    source: &str,
    expected_uri: &str,
    quickfix: &Value,
    wrong_kind: &Value,
    outside_range: &Value,
    editor: &str,
) {
    assert!(
        source.is_ascii(),
        "{editor} code-action fixture must be ASCII"
    );
    let actions = quickfix
        .as_array()
        .unwrap_or_else(|| panic!("{editor} quickfix result is not an array: {quickfix}"));
    assert_eq!(actions.len(), 1, "{editor} returned unexpected quickfixes");
    let action = &actions[0];
    assert_eq!(action["title"], "Insert missing `;`");
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["isPreferred"], true);
    assert!(
        action["command"].is_null(),
        "{editor} quickfix unexpectedly returned a command"
    );
    assert_eq!(action["diagnostics"][0]["code"], "ORNA-PARSE-002");

    let changes = action["edit"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("{editor} quickfix has no WorkspaceEdit changes: {action}"));
    assert_eq!(
        changes.len(),
        1,
        "{editor} quickfix changed an unexpected document set"
    );
    assert!(
        changes.contains_key(expected_uri),
        "{editor} quickfix changed a document other than {expected_uri}: {changes:?}"
    );
    let (uri, edits) = changes.iter().next().unwrap();
    let edits = edits
        .as_array()
        .unwrap_or_else(|| panic!("{editor} quickfix edits for {uri} are not an array"));
    assert_eq!(
        edits.len(),
        1,
        "{editor} quickfix should have one insertion edit"
    );
    let edit = &edits[0];
    assert_eq!(edit["newText"], ";");
    let insertion = source
        .find("value\n    intermediate")
        .expect("missing-semicolon source row")
        + "value".len();
    assert_eq!(
        edit["range"],
        range(source, insertion, insertion),
        "{editor} quickfix insertion point differs from the syntax-v1 repair"
    );
    let repaired = format!("{};{}", &source[..insertion], &source[insertion..]);
    assert!(
        orna_syntax_v1::parse_module(&repaired)
            .diagnostics
            .is_empty(),
        "{editor} quickfix did not repair the syntax-v1 fixture"
    );

    for (response, request_kind) in [
        (wrong_kind, "non-quickfix kind"),
        (outside_range, "out-of-range request"),
    ] {
        assert_eq!(
            response,
            &json!([]),
            "{editor} returned a quickfix for {request_kind}: {response}"
        );
    }
}

fn assert_signature(
    response: &Value,
    function: &str,
    active_parameter: u64,
    expected_parameters: &[&str],
    editor: &str,
) {
    let signatures = response["signatures"].as_array().unwrap_or_else(|| {
        panic!("{editor} signature-help response has no signatures for {function}: {response}")
    });
    assert_eq!(
        signatures.len(),
        1,
        "{editor} returned ambiguous signatures: {response}"
    );
    let signature = &signatures[0];
    assert!(
        signature["label"]
            .as_str()
            .is_some_and(|label| label.contains(&format!("fn {function}("))),
        "{editor} signature label omitted {function}: {response}"
    );
    assert_eq!(
        response["activeSignature"], 0,
        "{editor} active signature differs"
    );
    assert_eq!(response["activeParameter"], active_parameter);
    assert_eq!(signature["activeParameter"], active_parameter);
    let actual_parameters = signature["parameters"]
        .as_array()
        .unwrap_or_else(|| panic!("{editor} signature has no parameter list: {response}"))
        .iter()
        .map(|parameter| {
            parameter["label"]
                .as_str()
                .unwrap_or_else(|| panic!("{editor} parameter label is not a string: {parameter}"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual_parameters, expected_parameters,
        "{editor} signature parameter details differ for {function}"
    );
}

fn range(source: &str, start: usize, end: usize) -> Value {
    json!({
        "start": position_at(source, start),
        "end": position_at(source, end),
    })
}

fn position_at(source: &str, byte: usize) -> Value {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let character = prefix
        .rsplit_once('\n')
        .map_or(prefix.len(), |(_, tail)| tail.len());
    json!({ "line": line, "character": character })
}
