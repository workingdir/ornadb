use serde_json::Value;

pub const SOURCE: &str = include_str!("../fixtures/document-symbols-v1.orna");

const OUTCOME: &str =
    "pub enum Outcome {\n    success { value: Int },\n    failed { reason: Str },\n}";
const RENDERABLE: &str = "pub protocol Renderable {\n    fn render(self): Str;\n}";
const INVOICE: &str = "pub type Invoice {\n    id: Int,\n    impl Renderable {\n        fn render(self): Str = \"invoice\";\n    }\n}";
const RECEIPT: &str = "pub table Receipt(id: Int) {\n    total: Int,\n}";
const IDENTITY: &str = "pub fn identity(value: Int): Int = value;";

pub fn assert_contract(symbols: &Value, context: &str) {
    let roots = symbols
        .as_array()
        .unwrap_or_else(|| panic!("{context}: documentSymbol result is not an array: {symbols}"));
    assert_eq!(
        roots
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Outcome", "Renderable", "Invoice", "Receipt", "identity"],
        "{context}: top-level document outline"
    );

    let outcome = expect_symbol(roots, "Outcome", 10, context);
    assert_root_range(outcome, OUTCOME, context);
    assert_selection(outcome, "Outcome", 0, context);
    let variants = children(outcome, context);
    assert_eq!(
        names(variants),
        ["success", "failed"],
        "{context}: enum variants"
    );
    let success = expect_symbol(variants, "success", 22, context);
    assert_selection(success, "success", 0, context);
    let success_fields = children(success, context);
    assert_eq!(
        names(success_fields),
        ["value"],
        "{context}: success payload"
    );
    let value = expect_symbol(success_fields, "value", 8, context);
    assert_selection(value, "value", 0, context);
    assert_contained(&outcome["range"], &success["range"], context);
    assert_contained(&success["range"], &value["range"], context);

    let failed = expect_symbol(variants, "failed", 22, context);
    assert_selection(failed, "failed", 0, context);
    let failed_fields = children(failed, context);
    assert_eq!(
        names(failed_fields),
        ["reason"],
        "{context}: failed payload"
    );
    let reason = expect_symbol(failed_fields, "reason", 8, context);
    assert_selection(reason, "reason", 0, context);
    assert_contained(&failed["range"], &reason["range"], context);

    let renderable = expect_symbol(roots, "Renderable", 11, context);
    assert_root_range(renderable, RENDERABLE, context);
    assert_selection(renderable, "Renderable", 0, context);
    let renderable_methods = children(renderable, context);
    assert_eq!(
        names(renderable_methods),
        ["render"],
        "{context}: protocol methods"
    );
    let protocol_render = expect_symbol(renderable_methods, "render", 6, context);
    assert_selection(protocol_render, "render", 0, context);
    assert_contained(&renderable["range"], &protocol_render["range"], context);

    let invoice = expect_symbol(roots, "Invoice", 23, context);
    assert_root_range(invoice, INVOICE, context);
    assert_selection(invoice, "Invoice", 0, context);
    let invoice_children = children(invoice, context);
    assert_eq!(
        names(invoice_children),
        ["id", "render"],
        "{context}: type members"
    );
    let id = expect_symbol(invoice_children, "id", 8, context);
    assert_selection(id, "id", 0, context);
    let invoice_render = expect_symbol(invoice_children, "render", 6, context);
    assert_selection(invoice_render, "render", 1, context);
    for child in invoice_children {
        assert_contained(&invoice["range"], &child["range"], context);
    }

    let receipt = expect_symbol(roots, "Receipt", 5, context);
    assert_root_range(receipt, RECEIPT, context);
    assert_selection(receipt, "Receipt", 0, context);
    let receipt_children = children(receipt, context);
    assert_eq!(
        names(receipt_children),
        ["total"],
        "{context}: table fields"
    );
    let total = expect_symbol(receipt_children, "total", 8, context);
    assert_selection(total, "total", 0, context);
    assert_contained(&receipt["range"], &total["range"], context);

    let identity = expect_symbol(roots, "identity", 12, context);
    assert_root_range(identity, IDENTITY, context);
    assert_selection(identity, "identity", 0, context);
    assert!(
        identity["children"].is_null(),
        "{context}: function locals are not outline children"
    );
}

fn names(symbols: &[Value]) -> Vec<&str> {
    symbols
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect()
}

fn children<'a>(symbol: &'a Value, context: &str) -> &'a [Value] {
    symbol["children"].as_array().unwrap_or_else(|| {
        panic!(
            "{context}: {} has no outline children: {symbol}",
            symbol["name"]
        )
    })
}

fn expect_symbol<'a>(symbols: &'a [Value], name: &str, kind: u64, context: &str) -> &'a Value {
    let symbol = symbols
        .iter()
        .find(|symbol| symbol["name"] == name)
        .unwrap_or_else(|| panic!("{context}: missing {name} in {symbols:?}"));
    assert_eq!(symbol["kind"], kind, "{context}: wrong kind for {name}");
    assert_range_valid(&symbol["range"], context);
    assert_range_valid(&symbol["selectionRange"], context);
    assert_contained(&symbol["range"], &symbol["selectionRange"], context);
    symbol
}

fn assert_root_range(symbol: &Value, declaration: &str, context: &str) {
    let start = SOURCE
        .find(declaration)
        .unwrap_or_else(|| panic!("fixture is missing expected declaration {declaration}"));
    let end = start + declaration.len();
    assert_eq!(
        symbol["range"],
        range(SOURCE, start, end),
        "{context}: wrong source range for {}",
        symbol["name"]
    );
}

fn assert_selection(symbol: &Value, name: &str, occurrence: usize, context: &str) {
    let start = SOURCE
        .match_indices(name)
        .nth(occurrence)
        .unwrap_or_else(|| panic!("fixture does not contain {name} occurrence {occurrence}"))
        .0;
    assert_eq!(
        symbol["selectionRange"],
        range(SOURCE, start, start + name.len()),
        "{context}: wrong selection range for {name}"
    );
}

fn assert_range_valid(range: &Value, context: &str) {
    let start = position_tuple(&range["start"], context);
    let end = position_tuple(&range["end"], context);
    assert!(start <= end, "{context}: invalid range {range}");
}

fn assert_contained(outer: &Value, inner: &Value, context: &str) {
    let outer_start = position_tuple(&outer["start"], context);
    let outer_end = position_tuple(&outer["end"], context);
    let inner_start = position_tuple(&inner["start"], context);
    let inner_end = position_tuple(&inner["end"], context);
    assert!(
        outer_start <= inner_start && inner_end <= outer_end,
        "{context}: nested outline range {inner} escapes parent range {outer}"
    );
}

fn position_tuple(position: &Value, context: &str) -> (u64, u64) {
    (
        position["line"]
            .as_u64()
            .unwrap_or_else(|| panic!("{context}: invalid line in {position}")),
        position["character"]
            .as_u64()
            .unwrap_or_else(|| panic!("{context}: invalid character in {position}")),
    )
}

fn range(source: &str, start: usize, end: usize) -> Value {
    serde_json::json!({
        "start": position(source, start),
        "end": position(source, end),
    })
}

fn position(source: &str, byte: usize) -> Value {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .encode_utf16()
        .count();
    serde_json::json!({ "line": line, "character": column })
}
