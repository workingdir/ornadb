//! Declaration order of table columns survives semantic analysis.
//!
//! `sys.Table.columns` reports a table's columns in the order the declaration
//! lists them (Orna-1.1.0 `sys.Table` / `sys.Column`). The semantic schema
//! keeps fields in a map, which sorts by name, so the order is carried beside
//! the map instead of being derived from it.

use orna_semantic_v1::{ModuleInput, SymbolKind, analyze};

fn analysis() -> orna_semantic_v1::Analysis {
    analyze(&[ModuleInput::new(
        "contacts/main.orna",
        include_str!("fixtures/table-key-surface.orna"),
    )])
}

fn schema(analysis: &orna_semantic_v1::Analysis, table: &str) -> orna_semantic_v1::TableSchema {
    let module = analysis
        .modules
        .values()
        .next()
        .expect("fixture module is present");
    let symbol = module.symbols.get(table).expect("table is declared");
    assert_eq!(symbol.kind, SymbolKind::Table);
    symbol.table_schema.clone().expect("table schema")
}

#[test]
// Specified: Orna-1.1.0 sys.Table.columns reports declaration order.
fn table_schema_retains_declaration_order_of_columns() {
    let analysis = analysis();
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);

    // The fixture declares `value` before `label`; sorting by name would
    // report `label` first.
    let reading = schema(&analysis, "Reading");
    assert_eq!(
        reading.declaration_order,
        ["sensor", "sequence", "value", "label"],
        "key columns come first, then members, in declaration order",
    );
    let sorted: Vec<&String> = reading.fields.keys().collect();
    assert_eq!(
        sorted,
        ["label", "sensor", "sequence", "value"],
        "the field map still sorts by name, which is why the order is carried separately",
    );
    assert_eq!(
        reading.declaration_order.len(),
        reading.fields.len(),
        "every visible field carries exactly one position",
    );

    // A single-column table keeps its one position.
    let auto_note = schema(&analysis, "AutoNote");
    assert_eq!(auto_note.declaration_order, ["text", "label", "display"]);
}
