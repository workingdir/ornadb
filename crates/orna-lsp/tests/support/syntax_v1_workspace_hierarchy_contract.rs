//! Shared syntax-v1 editor contracts for workspace symbols and call hierarchy.

use serde_json::{Value, json};

/// Positions used by editor clients and the protocol proof. The fixtures are
/// crate-local `include_str!` inputs, so their text and request coordinates stay
/// identical across all three editor attachment probes.
pub fn request_data(provider_source: &str, caller_source: &str) -> Value {
    json!({
        "root_definition": position_at(provider_source, after(provider_source, "pub fn root", "pub fn ")),
        "seed_definition": position_at(provider_source, after(provider_source, "pub fn seed", "pub fn ")),
        "shadowed_definition": position_at(provider_source, after(provider_source, "pub fn shadowed", "pub fn shadowed")),
        "unresolved_definition": position_at(provider_source, after(provider_source, "pub fn unresolved", "pub fn unresolved")),
        "root_reference": position_at(caller_source, after(caller_source, "root(value)", "")),
        "ambiguous_reference": position_at(caller_source, after(caller_source, "collide(value)", "")),
    })
}

/// Proves stable workspace-symbol results plus cross-file call-hierarchy
/// preparation, incoming/outgoing edges, exact call ranges, and fail-closed
/// behavior for shadowed, unresolved, and ambiguous references.
pub fn assert_contract(
    provider_source: &str,
    caller_source: &str,
    provider_uri: &str,
    caller_uri: &str,
    evidence: &Value,
    editor: &str,
) {
    let mid = symbols(&evidence["workspace_mid"], &["mid", "remote_mid"], editor);
    assert_eq!(
        evidence["workspace_mid_repeat"], evidence["workspace_mid"],
        "{editor} workspace-symbol order changed for an identical query"
    );
    assert_eq!(
        mid.iter()
            .map(|symbol| symbol["location"]["uri"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [provider_uri, caller_uri],
        "{editor} `mid` symbols came from unexpected documents"
    );
    for symbol in mid {
        assert_eq!(
            symbol["kind"], 12,
            "{editor} symbol is not a function: {symbol}"
        );
    }

    let prefix = symbols(
        &evidence["workspace_rem"],
        &["remote_mid", "remote_root"],
        editor,
    );
    assert_eq!(
        prefix
            .iter()
            .map(|symbol| symbol["location"]["uri"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [caller_uri, caller_uri],
        "{editor} prefix search crossed unexpected documents"
    );
    let fuzzy = symbols(&evidence["workspace_remi"], &["remote_mid"], editor);
    assert_eq!(fuzzy[0]["location"]["uri"], caller_uri);

    let root_item = one(&evidence["call_root_item"], editor, "root prepare");
    assert_eq!(
        root_item["name"], "root",
        "{editor} prepared the wrong root item"
    );
    assert_eq!(
        root_item["uri"], provider_uri,
        "{editor} root item URI differs"
    );
    let root_start = after(provider_source, "pub fn root", "pub fn ");
    assert_eq!(
        root_item["selectionRange"],
        range_at(provider_source, root_start, root_start + "root".len()),
        "{editor} root selection range differs"
    );

    let outgoing = evidence["call_root_outgoing"]
        .as_array()
        .unwrap_or_else(|| panic!("{editor} root outgoing result is not an array"));
    assert_eq!(
        outgoing.len(),
        2,
        "{editor} root outgoing edges differ: {outgoing:?}"
    );
    assert_eq!(outgoing[0]["to"]["name"], "seed");
    assert_eq!(outgoing[0]["to"]["uri"], provider_uri);
    assert_eq!(outgoing[1]["to"]["name"], "mid");
    assert_eq!(outgoing[1]["to"]["uri"], provider_uri);
    let seed_offsets = [
        after(provider_source, "mid(seed(value))", "mid("),
        after(provider_source, "+ seed(value)", "+ "),
    ];
    assert_eq!(
        outgoing[0]["fromRanges"],
        json!([
            range_at(
                provider_source,
                seed_offsets[0],
                seed_offsets[0] + "seed".len()
            ),
            range_at(
                provider_source,
                seed_offsets[1],
                seed_offsets[1] + "seed".len()
            ),
        ]),
        "{editor} root-to-seed call ranges differ"
    );
    let mid_offset = provider_source
        .find("mid(seed(value))")
        .expect("root-to-mid call in workspace hierarchy fixture");
    assert_eq!(
        outgoing[1]["fromRanges"],
        json!([range_at(
            provider_source,
            mid_offset,
            mid_offset + "mid".len()
        )]),
        "{editor} root-to-mid call range differs"
    );

    let incoming = evidence["call_root_incoming"]
        .as_array()
        .unwrap_or_else(|| panic!("{editor} root incoming result is not an array"));
    assert_eq!(
        incoming.len(),
        2,
        "{editor} root incoming edges differ: {incoming:?}"
    );
    assert_eq!(incoming[0]["from"]["name"], "remote_root");
    assert_eq!(incoming[1]["from"]["name"], "remote_mid");
    for call in incoming {
        assert_eq!(call["from"]["uri"], caller_uri);
        assert_eq!(call["fromRanges"].as_array().unwrap().len(), 1);
    }
    let caller_root_offsets = caller_source
        .match_indices("root(value)")
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    assert_eq!(caller_root_offsets.len(), 2);
    assert_eq!(
        incoming[0]["fromRanges"],
        json!([range_at(
            caller_source,
            caller_root_offsets[0],
            caller_root_offsets[0] + "root".len()
        )]),
        "{editor} remote_root call range differs"
    );
    assert_eq!(
        incoming[1]["fromRanges"],
        json!([range_at(
            caller_source,
            caller_root_offsets[1],
            caller_root_offsets[1] + "root".len()
        )]),
        "{editor} remote_mid call range differs"
    );

    let seed_item = one(&evidence["call_seed_item"], editor, "seed prepare");
    assert_eq!(seed_item["name"], "seed");
    let seed_incoming = evidence["call_seed_incoming"].as_array().unwrap();
    assert_eq!(
        seed_incoming.len(),
        2,
        "{editor} seed incoming edges differ"
    );
    assert_eq!(seed_incoming[0]["from"]["name"], "mid");
    assert_eq!(seed_incoming[1]["from"]["name"], "root");
    assert_eq!(seed_incoming[0]["from"]["uri"], provider_uri);
    assert_eq!(seed_incoming[1]["from"]["uri"], provider_uri);
    assert_eq!(seed_incoming[0]["fromRanges"].as_array().unwrap().len(), 1);
    assert_eq!(seed_incoming[1]["fromRanges"].as_array().unwrap().len(), 2);

    assert_eq!(
        one(
            &evidence["call_root_reference"],
            editor,
            "cross-file root prepare"
        )["uri"],
        provider_uri,
        "{editor} did not resolve the caller reference to the provider"
    );
    assert!(
        evidence["call_ambiguous_reference"].is_null(),
        "{editor} prepared an ambiguous cross-file reference: {}",
        evidence["call_ambiguous_reference"]
    );
    for (key, label) in [
        ("call_shadowed_outgoing", "shadowed local call"),
        ("call_unresolved_outgoing", "unresolved call"),
    ] {
        assert_eq!(
            evidence[key],
            json!([]),
            "{editor} emitted an outgoing edge for {label}"
        );
    }
}

fn symbols<'a>(response: &'a Value, names: &[&str], editor: &str) -> &'a [Value] {
    let symbols = response.as_array().unwrap_or_else(|| {
        panic!("{editor} workspace-symbol response is not an array: {response}")
    });
    assert_eq!(
        symbols
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        names,
        "{editor} workspace-symbol ranking differs: {response}"
    );
    symbols
}

fn one<'a>(response: &'a Value, editor: &str, request: &str) -> &'a Value {
    let items = response
        .as_array()
        .unwrap_or_else(|| panic!("{editor} {request} response is not an array: {response}"));
    assert_eq!(
        items.len(),
        1,
        "{editor} {request} did not return one item: {response}"
    );
    &items[0]
}

fn after(source: &str, needle: &str, suffix: &str) -> usize {
    source
        .find(needle)
        .expect("workspace hierarchy fixture text")
        + suffix.len()
}

fn range_at(source: &str, start: usize, end: usize) -> Value {
    json!({"start": position_at(source, start), "end": position_at(source, end)})
}

fn position_at(source: &str, byte: usize) -> Value {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let character = prefix
        .rsplit_once('\n')
        .map_or(prefix.len(), |(_, tail)| tail.encode_utf16().count());
    json!({"line": line, "character": character})
}
