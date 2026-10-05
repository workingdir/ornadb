//! Shared syntax-v1 document-symbol contract for protocol and editor probes.

use serde_json::{Value, json};

#[derive(Clone, Copy)]
struct ExpectedSymbol {
    name: &'static str,
    kind: u64,
    declaration_prefix: &'static str,
    full_end: &'static str,
}

const PROVIDER_SYMBOLS: &[ExpectedSymbol] = &[
    ExpectedSymbol {
        name: "Renderable",
        kind: 11,
        declaration_prefix: "pub protocol Renderable",
        full_end: "    fn render(self): Str;\n}",
    },
    ExpectedSymbol {
        name: "Clash",
        kind: 11,
        declaration_prefix: "pub protocol Clash",
        full_end: "    fn conflict(self): Str;\n}",
    },
    ExpectedSymbol {
        name: "Document",
        kind: 23,
        declaration_prefix: "pub type Document",
        full_end: "        fn render(self): Str = \"document\";\n    }\n}",
    },
    ExpectedSymbol {
        name: "Invoice",
        kind: 5,
        declaration_prefix: "pub table Invoice",
        full_end: "        fn render(self): Str = \"invoice\";\n    }\n}",
    },
    ExpectedSymbol {
        name: "Orphan",
        kind: 23,
        declaration_prefix: "pub type Orphan",
        full_end: "        fn missing(self): Str = \"orphan\";\n    }\n}",
    },
    ExpectedSymbol {
        name: "DocumentAlias",
        kind: 23,
        declaration_prefix: "pub type DocumentAlias",
        full_end: "pub type DocumentAlias = Document;",
    },
];

const CALLER_SYMBOLS: &[ExpectedSymbol] = &[
    ExpectedSymbol {
        name: "Clash",
        kind: 11,
        declaration_prefix: "pub protocol Clash",
        full_end: "    fn conflict(self): Str;\n}",
    },
    ExpectedSymbol {
        name: "Remote",
        kind: 23,
        declaration_prefix: "pub type Remote",
        full_end: "        fn render(self): Str = \"remote\";\n    }\n}",
    },
    ExpectedSymbol {
        name: "AmbiguousConsumer",
        kind: 23,
        declaration_prefix: "pub type AmbiguousConsumer",
        full_end: "        fn conflict(self): Str = \"ambiguous\";\n    }\n}",
    },
    ExpectedSymbol {
        name: "accept",
        kind: 12,
        declaration_prefix: "pub fn accept",
        full_end: "pub fn accept(value: Document): Document = value;",
    },
];

pub fn assert_contract(
    provider_source: &str,
    caller_source: &str,
    provider_symbols: &Value,
    provider_symbols_repeat: &Value,
    caller_symbols: &Value,
    caller_symbols_repeat: &Value,
    editor: &str,
) {
    assert_eq!(
        provider_symbols, provider_symbols_repeat,
        "{editor} provider document-symbol response changed between identical requests"
    );
    assert_eq!(
        caller_symbols, caller_symbols_repeat,
        "{editor} caller document-symbol response changed between identical requests"
    );
    assert_symbols(
        provider_source,
        provider_symbols,
        PROVIDER_SYMBOLS,
        editor,
        "provider",
    );
    assert_symbols(
        caller_source,
        caller_symbols,
        CALLER_SYMBOLS,
        editor,
        "caller",
    );
}

fn assert_symbols(
    source: &str,
    response: &Value,
    expected: &[ExpectedSymbol],
    editor: &str,
    document: &str,
) {
    let symbols = response.as_array().unwrap_or_else(|| {
        panic!("{editor} {document} document symbols are not an array: {response}")
    });
    assert_eq!(
        symbols.len(),
        expected.len(),
        "{editor} {document} document-symbol inventory differs: {response}"
    );

    for (symbol, expected) in symbols.iter().zip(expected) {
        let declaration_start = source
            .find(expected.declaration_prefix)
            .unwrap_or_else(|| panic!("fixture lost declaration {}", expected.name));
        let selection_start = declaration_start
            + source[declaration_start..]
                .find(expected.name)
                .expect("symbol name inside declaration");
        let full_end = declaration_start
            + source[declaration_start..]
                .find(expected.full_end)
                .unwrap_or_else(|| panic!("fixture lost full declaration {}", expected.name))
            + expected.full_end.len();
        let selection_end = selection_start + expected.name.len();

        assert_eq!(
            symbol["name"], expected.name,
            "{editor} {document}: {symbol}"
        );
        assert_eq!(
            symbol["kind"], expected.kind,
            "{editor} {document}: {symbol}"
        );
        assert_eq!(
            symbol["range"],
            json!({
                "start": super::position_at(source, declaration_start),
                "end": super::position_at(source, full_end),
            }),
            "{editor} {document} {} full range differs: {symbol}",
            expected.name
        );
        assert_eq!(
            symbol["selectionRange"],
            json!({
                "start": super::position_at(source, selection_start),
                "end": super::position_at(source, selection_end),
            }),
            "{editor} {document} {} selection range differs: {symbol}",
            expected.name
        );
    }
}
