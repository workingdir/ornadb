//! Shared syntax-v1 type-hierarchy and moniker contracts for protocol and
//! editor attachment proofs.

use serde_json::{Value, json};

/// Request positions derived from the crate-local fixture text. These values
/// are shared by the protocol, Neovim, and Emacs probes.
pub fn request_data(provider_source: &str, caller_source: &str) -> Value {
    json!({
        "document_declaration": position_at(
            provider_source,
            after(provider_source, "pub type Document", "pub type "),
        ),
        "document_reference": position_at(
            caller_source,
            after(caller_source, "value: Document", "value: "),
        ),
        "renderable_declaration": position_at(
            provider_source,
            after(provider_source, "pub protocol Renderable", "pub protocol "),
        ),
        "orphan_declaration": position_at(
            provider_source,
            after(provider_source, "pub type Orphan", "pub type "),
        ),
        "ambiguous_consumer_declaration": position_at(
            caller_source,
            after(caller_source, "pub type AmbiguousConsumer", "pub type "),
        ),
        "clash_provider_declaration": position_at(
            provider_source,
            after(provider_source, "pub protocol Clash", "pub protocol "),
        ),
        "clash_caller_declaration": position_at(
            caller_source,
            after(caller_source, "pub type Clash", "pub type "),
        ),
        "document_alias_reference": position_at(
            provider_source,
            after(provider_source, "DocumentAlias = Document", "DocumentAlias = "),
        ),
        "local_parameter_declaration": position_at(
            caller_source,
            after(caller_source, "value: Document", "v"),
        ),
        "local_parameter_use": position_at(
            caller_source,
            after(caller_source, "= value", "= "),
        ),
        "ambiguous_reference": position_at(
            caller_source,
            after(caller_source, "impl Clash", "impl "),
        ),
    })
}

/// Proves cross-file hierarchy lookup, exact declaration ranges, and project
/// versus document moniker identity. The same captured JSON is asserted for
/// the raw protocol client and both real editor clients.
pub fn assert_contract(
    provider_source: &str,
    caller_source: &str,
    provider_uri: &str,
    caller_uri: &str,
    evidence: &Value,
    editor: &str,
) {
    let document_start = after(provider_source, "pub type Document", "pub type ");
    let document = one(&evidence["type_document_item"], editor, "Document prepare");
    assert_item(
        document,
        provider_source,
        provider_uri,
        "Document",
        document_start,
        "Document".len(),
        editor,
    );
    let referenced_document = one(
        &evidence["type_document_reference"],
        editor,
        "cross-file Document prepare",
    );
    assert_item(
        referenced_document,
        provider_source,
        provider_uri,
        "Document",
        document_start,
        "Document".len(),
        editor,
    );

    let document_supers = items(
        &evidence["type_document_supertypes"],
        editor,
        "Document supertype",
    );
    assert_eq!(
        document_supers.len(),
        1,
        "{editor} Document supertypes differ"
    );
    let renderable_start = after(provider_source, "pub protocol Renderable", "pub protocol ");
    assert_item(
        &document_supers[0],
        provider_source,
        provider_uri,
        "Renderable",
        renderable_start,
        "Renderable".len(),
        editor,
    );

    let renderable = one(
        &evidence["type_renderable_item"],
        editor,
        "Renderable prepare",
    );
    assert_item(
        renderable,
        provider_source,
        provider_uri,
        "Renderable",
        renderable_start,
        "Renderable".len(),
        editor,
    );
    let subtypes = items(
        &evidence["type_renderable_subtypes"],
        editor,
        "Renderable subtypes",
    );
    assert_eq!(
        subtypes
            .iter()
            .map(|item| item["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Remote", "Document", "Invoice"],
        "{editor} subtype order differs"
    );
    for (item, source, uri, name, needle, prefix) in [
        (
            &subtypes[0],
            caller_source,
            caller_uri,
            "Remote",
            "pub type Remote",
            "pub type ",
        ),
        (
            &subtypes[1],
            provider_source,
            provider_uri,
            "Document",
            "pub type Document",
            "pub type ",
        ),
        (
            &subtypes[2],
            provider_source,
            provider_uri,
            "Invoice",
            "pub table Invoice",
            "pub table ",
        ),
    ] {
        let start = after(source, needle, prefix);
        assert_item(item, source, uri, name, start, name.len(), editor);
    }

    assert_empty(
        &evidence["type_orphan_supertypes"],
        editor,
        "unresolved Orphan",
    );
    assert_empty(
        &evidence["type_ambiguous_consumer_supertypes"],
        editor,
        "ambiguous consumer",
    );
    assert_empty(
        &evidence["type_clash_subtypes"],
        editor,
        "duplicate Clash subtypes",
    );

    let declaration = one(
        &evidence["type_document_moniker"],
        editor,
        "Document moniker",
    );
    assert_project_export(declaration, editor, "Document declaration");
    let declaration_identifier = identifier(declaration, editor, "Document declaration");
    let imported = one(
        &evidence["type_imported_moniker"],
        editor,
        "imported Document moniker",
    );
    assert_project_moniker(imported, editor, "imported Document");
    assert_eq!(imported["kind"], "import", "{editor} import kind differs");
    assert_eq!(
        identifier(imported, editor, "imported Document"),
        declaration_identifier,
        "{editor} imported Document moniker lost declaration identity"
    );

    let same_file = one(
        &evidence["type_same_file_moniker"],
        editor,
        "same-file Document moniker",
    );
    assert_project_moniker(same_file, editor, "same-file Document");
    assert_eq!(
        same_file["kind"], "local",
        "{editor} same-file use kind differs"
    );
    assert_eq!(
        identifier(same_file, editor, "same-file Document"),
        declaration_identifier,
        "{editor} same-file Document use lost declaration identity"
    );

    let provider_clash = one(
        &evidence["type_provider_clash_moniker"],
        editor,
        "provider Clash moniker",
    );
    let caller_clash = one(
        &evidence["type_caller_clash_moniker"],
        editor,
        "caller Clash moniker",
    );
    assert_project_export(provider_clash, editor, "provider Clash declaration");
    assert_project_export(caller_clash, editor, "caller Clash declaration");
    assert_ne!(
        identifier(provider_clash, editor, "provider Clash"),
        identifier(caller_clash, editor, "caller Clash"),
        "{editor} duplicate names share a project moniker"
    );

    let local = one(
        &evidence["type_local_parameter_moniker"],
        editor,
        "local parameter moniker",
    );
    let local_use = one(
        &evidence["type_local_use_moniker"],
        editor,
        "local parameter use moniker",
    );
    for (moniker, label) in [
        (local, "local parameter"),
        (local_use, "local parameter use"),
    ] {
        assert_eq!(
            moniker["scheme"], "orna-syntax-v1",
            "{editor} {label} scheme differs"
        );
        assert_eq!(
            moniker["unique"], "document",
            "{editor} {label} scope differs"
        );
        assert_eq!(moniker["kind"], "local", "{editor} {label} kind differs");
        assert!(
            identifier(moniker, editor, label).contains("#local:"),
            "{editor} {label} identifier lacks its document-local key"
        );
    }
    assert_eq!(
        identifier(local, editor, "local parameter"),
        identifier(local_use, editor, "local parameter use"),
        "{editor} local parameter and use lost document identity"
    );
    assert!(
        evidence["type_ambiguous_moniker"].is_null(),
        "{editor} emitted a moniker for an ambiguous reference: {}",
        evidence["type_ambiguous_moniker"]
    );
}

fn assert_project_export(moniker: &Value, editor: &str, label: &str) {
    assert_project_moniker(moniker, editor, label);
    assert_eq!(moniker["kind"], "export", "{editor} {label} kind differs");
}

fn assert_project_moniker(moniker: &Value, editor: &str, label: &str) {
    assert_eq!(
        moniker["scheme"], "orna-syntax-v1",
        "{editor} {label} scheme differs"
    );
    assert_eq!(
        moniker["unique"], "project",
        "{editor} {label} scope differs"
    );
    assert!(
        identifier(moniker, editor, label).contains('#'),
        "{editor} {label} identifier has no declaration key"
    );
}

fn identifier<'a>(moniker: &'a Value, editor: &str, label: &str) -> &'a str {
    moniker["identifier"]
        .as_str()
        .filter(|identifier| !identifier.is_empty())
        .unwrap_or_else(|| panic!("{editor} {label} has no identifier: {moniker}"))
}

fn assert_item(
    item: &Value,
    source: &str,
    uri: &str,
    name: &str,
    start: usize,
    length: usize,
    editor: &str,
) {
    assert_eq!(item["name"], name, "{editor} hierarchy item name differs");
    assert_eq!(item["uri"], uri, "{editor} {name} hierarchy URI differs");
    assert_eq!(
        item["selectionRange"],
        range_at(source, start, start + length),
        "{editor} {name} selection range differs"
    );
}

fn assert_empty(response: &Value, editor: &str, label: &str) {
    assert_eq!(
        items(response, editor, label).len(),
        0,
        "{editor} {label} was not empty"
    );
}

fn items<'a>(response: &'a Value, editor: &str, label: &str) -> &'a [Value] {
    response
        .as_array()
        .unwrap_or_else(|| panic!("{editor} {label} response is not an array: {response}"))
}

fn one<'a>(response: &'a Value, editor: &str, label: &str) -> &'a Value {
    let values = items(response, editor, label);
    assert_eq!(
        values.len(),
        1,
        "{editor} {label} did not return one result: {response}"
    );
    &values[0]
}

fn after(source: &str, needle: &str, prefix: &str) -> usize {
    source.find(needle).expect("type hierarchy fixture text") + prefix.len()
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
