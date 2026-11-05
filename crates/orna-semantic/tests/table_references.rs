//! Stored table references are resolved from table columns.
//!
//! Orna-1.1.0, "Stored table references": a table type used as a field denotes
//! a stored reference to a row of that table, and it does not embed a copy of
//! the row (ORNA-REF-001). ORNA-REF-003 says a non-optional reference defaults
//! to `restrict` on target delete or re-key.

use orna_semantic::{ModuleInput, ReferenceAction, SymbolKind, analyze};

fn analysis() -> orna_semantic::Analysis {
    analyze(&[ModuleInput::new(
        "garage.orna",
        include_str!("fixtures/table-references.orna"),
    )])
}

#[test]
// Specified: ORNA-REF-001, ORNA-REF-003.
fn table_columns_that_name_a_table_are_stored_references() {
    let analysis = analysis();
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
    let module = analysis
        .modules
        .values()
        .next()
        .expect("fixture module is present");

    let vehicle = module.symbols.get("Vehicle").expect("table is declared");
    assert_eq!(vehicle.kind, SymbolKind::Table);
    let schema = vehicle.table_schema.as_ref().expect("table schema");

    // `owner: Contact` is a stored reference; `spare: Contact?` is an
    // optional one. `plate: Str` names no table and is not a reference.
    let references: Vec<(&str, &str, bool, ReferenceAction)> = schema
        .references
        .iter()
        .map(|reference| {
            (
                reference.from_column.as_str(),
                reference.to_table.as_str(),
                reference.optional,
                reference.on_delete,
            )
        })
        .collect();
    assert_eq!(
        references,
        [
            ("owner", "garage.Contact", false, ReferenceAction::Restrict),
            ("spare", "garage.Contact", true, ReferenceAction::SetNone),
        ],
        "table-typed columns become references; the target is the qualified identity",
    );
    assert!(
        schema
            .references
            .iter()
            .all(|reference| reference.on_rekey == reference.on_delete),
        "re-key follows the same default as delete (ORNA-REF-003)",
    );

    // A table with no table-typed column carries no references, so
    // `sys.Table.references` is the empty set for it rather than a guess.
    let contact = module.symbols.get("Contact").expect("table is declared");
    let contact_schema = contact.table_schema.as_ref().expect("table schema");
    assert!(contact_schema.references.is_empty());
}
