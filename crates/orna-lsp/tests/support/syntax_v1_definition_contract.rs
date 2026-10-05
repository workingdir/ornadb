//! Shared syntax-v1 go-to-definition contract for protocol and editor probes.

use serde_json::{Value, json};

/// Request positions are derived from the two fixture files embedded by the
/// conformance test with `include_str!`.
pub fn request_data(provider_source: &str, caller_source: &str) -> Value {
    json!({
        "cross_file_reference": position_at(
            caller_source,
            after(caller_source, "= root(value)", "= "),
        ),
        "shadowed_reference": position_at(
            provider_source,
            after(provider_source, "= seed(1)", "= "),
        ),
        "shadowed_parameter": position_at(
            provider_source,
            after(provider_source, "pub fn shadowed(seed", "pub fn shadowed("),
        ),
        "unresolved_reference": position_at(
            provider_source,
            after(provider_source, "= missing(value)", "= "),
        ),
        "ambiguous_reference": position_at(
            caller_source,
            after(caller_source, "= collide(value)", "= "),
        ),
    })
}

/// Checks exact target documents and token ranges, including fail-closed
/// behavior when a reference is ambiguous or has no declaration.
pub fn assert_contract(
    provider_source: &str,
    caller_source: &str,
    provider_uri: &str,
    caller_uri: &str,
    evidence: &Value,
    editor: &str,
) {
    assert_ne!(
        provider_uri, caller_uri,
        "{editor} fixtures must use distinct URIs"
    );
    assert!(caller_source.contains("= root(value)"));
    assert!(caller_source.contains("= collide(value)"));
    assert!(provider_source.contains("= seed(1)"));
    assert!(provider_source.contains("= missing(value)"));
    assert_location(
        &evidence["definition_cross_file"],
        provider_source,
        provider_uri,
        after(provider_source, "pub fn root", "pub fn "),
        "root",
        editor,
        "cross-file root call",
    );
    assert_location(
        &evidence["definition_shadowed_parameter"],
        provider_source,
        provider_uri,
        after(provider_source, "pub fn shadowed(seed", "pub fn shadowed("),
        "seed",
        editor,
        "parameter-shadowed seed call",
    );
    for (key, label) in [
        ("definition_unresolved", "unresolved missing call"),
        ("definition_ambiguous", "ambiguous collide call"),
    ] {
        assert!(
            evidence[key].is_null(),
            "{editor} returned a definition for {label}: {}",
            evidence[key]
        );
    }
}

fn assert_location(
    location: &Value,
    source: &str,
    uri: &str,
    start: usize,
    name: &str,
    editor: &str,
    label: &str,
) {
    assert_eq!(location["uri"], uri, "{editor} {label} target URI differs");
    assert_eq!(
        location["range"],
        range_at(source, start, start + name.len()),
        "{editor} {label} target range differs"
    );
}

fn after(source: &str, needle: &str, prefix: &str) -> usize {
    source.find(needle).expect("definition fixture text") + prefix.len()
}

fn position_at(source: &str, offset: usize) -> Value {
    let before = &source[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count();
    let character_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = before[character_start..].encode_utf16().count();
    json!({"line": line, "character": character})
}

fn range_at(source: &str, start: usize, end: usize) -> Value {
    json!({"start": position_at(source, start), "end": position_at(source, end)})
}
