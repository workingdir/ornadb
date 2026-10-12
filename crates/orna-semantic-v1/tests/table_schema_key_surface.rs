use orna_semantic_v1::{
    DIAG_TYPE, ModuleInput, RowUnitInput, SymbolKind, Type, admit_row_unit, analyze,
};

fn analysis() -> orna_semantic_v1::Analysis {
    analyze(&[ModuleInput::new(
        "contacts/main.orna",
        include_str!("fixtures/table-key-surface.orna"),
    )])
}

fn admit(
    analysis: &orna_semantic_v1::Analysis,
    table: &str,
    key_path: &[&str],
    source: &str,
) -> orna_semantic_v1::RowUnitAdmission {
    let table_path = format!("contacts/{table}");
    let key_path = key_path.iter().map(|part| (*part).to_owned()).collect::<Vec<_>>();
    let logical_path = format!("{table_path}/{}", key_path.join("/"));
    admit_row_unit(
        analysis,
        &RowUnitInput {
            logical_path: &logical_path,
            table_path: &table_path,
            key_path: &key_path,
            source,
            source_bytes: source.as_bytes(),
            parse_as: "row_unit",
        },
    )
}

#[test]
fn table_schema_exposes_automatic_id_and_checks_key_defaults_and_computed_self() {
    let analysis = analysis();
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
    let module = analysis
        .modules
        .values()
        .next()
        .expect("fixture module is present");
    let auto_note = module.symbols.get("AutoNote").expect("table is declared");
    assert_eq!(auto_note.kind, SymbolKind::Table);
    let schema = auto_note.table_schema.as_ref().expect("table schema");
    assert_eq!(schema.fields.get("id"), Some(&Type::Int));
    let admission = schema.admission.as_ref().expect("admission metadata");
    assert!(admission.automatic_key);
    assert_eq!(admission.keys, [("id".into(), Type::Int)]);

    let invalid = analyze(&[ModuleInput::new(
        "contacts/invalid.orna",
        include_str!("fixtures/table-key-invalid.orna"),
    )]);
    assert!(!invalid.is_ok());
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "type has no canonical primary-key encoding"
    }));
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "static types are incompatible"
    }));
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == orna_semantic_v1::DIAG_DUPLICATE
            && diagnostic
                .message()
                .contains("automatic primary-key field `id` conflicts")
    }));
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "primary-key fields must declare a type"
    }));
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == orna_semantic_v1::DIAG_DUPLICATE
            && diagnostic
                .message()
                .contains("table field conflicts with another field or primary key")
    }));
}

#[test]
fn key_paths_reconstruct_ordered_composite_tuple_reference_and_enum_components() {
    let analysis = analysis();
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);

    let automatic = admit(
        &analysis,
        "AutoNote",
        &["7.orna"],
        include_str!("fixtures/table-row-auto-note.orna"),
    );
    assert!(automatic.is_ok(), "{:#?}", automatic.diagnostics);
    assert_eq!(automatic.key, ["7"]);

    let composite = admit(
        &analysis,
        "Reading",
        &["north", "42.orna"],
        include_str!("fixtures/table-row-reading.orna"),
    );
    assert!(composite.is_ok(), "{:#?}", composite.diagnostics);
    assert_eq!(composite.key, ["north", "42"]);

    let tuple = admit(
        &analysis,
        "TupleReading",
        &["north", "42.orna"],
        include_str!("fixtures/table-row-tuple.orna"),
    );
    assert!(tuple.is_ok(), "{:#?}", tuple.diagnostics);
    assert_eq!(tuple.key, ["north", "42"]);

    let reference = admit(
        &analysis,
        "Child",
        &["42.orna"],
        include_str!("fixtures/table-row-child.orna"),
    );
    assert!(reference.is_ok(), "{:#?}", reference.diagnostics);
    assert_eq!(reference.key, ["42"]);

    let alias = admit(
        &analysis,
        "AliasKey",
        &["42.orna"],
        include_str!("fixtures/table-row-text.orna"),
    );
    assert!(alias.is_ok(), "{:#?}", alias.diagnostics);
    assert_eq!(alias.key, ["42"]);

    let aliased_reference = admit(
        &analysis,
        "AliasChild",
        &["42.orna"],
        include_str!("fixtures/table-row-text.orna"),
    );
    assert!(
        aliased_reference.is_ok(),
        "{:#?}",
        aliased_reference.diagnostics
    );
    assert_eq!(aliased_reference.key, ["42"]);

    let enum_key = admit(
        &analysis,
        "Colored",
        &["blue.orna"],
        include_str!("fixtures/table-row-text.orna"),
    );
    assert!(enum_key.is_ok(), "{:#?}", enum_key.diagnostics);
    assert_eq!(enum_key.key, ["blue"]);
    let invalid_enum = admit(
        &analysis,
        "Colored",
        &["green.orna"],
        include_str!("fixtures/table-row-text.orna"),
    );
    assert!(!invalid_enum.is_ok());
}

#[test]
fn path_keys_require_canonical_typed_date_instant_decimal_and_uuid_text() {
    let analysis = analysis();
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);

    for (table, key) in [
        ("Dated", "2024-02-29.orna"),
        ("Timed", "2024-02-29T00~3a00~3a00Z.orna"),
        ("DecimalKey", "15e-1.decimal.orna"),
        (
            "UuidKey",
            "123e4567-e89b-12d3-a456-426614174000.orna",
        ),
    ] {
        let row = admit(
            &analysis,
            table,
            &[key],
            include_str!("fixtures/table-row-text.orna"),
        );
        assert!(row.is_ok(), "{table} {key}: {:#?}", row.diagnostics);
    }

    for (table, key) in [
        ("Dated", "2023-02-29.orna"),
        (
            "Timed",
            "2024-02-29T01~3a00~3a00~2b01~3a00.orna",
        ),
        ("DecimalKey", "150e-2.decimal.orna"),
        (
            "UuidKey",
            "123E4567-E89B-12D3-A456-426614174000.orna",
        ),
    ] {
        let row = admit(
            &analysis,
            table,
            &[key],
            include_str!("fixtures/table-row-text.orna"),
        );
        assert!(!row.is_ok(), "unexpectedly admitted {table} {key}");
    }

    let invalid_integer = admit(
        &analysis,
        "Parent",
        &["-0.orna"],
        include_str!("fixtures/table-row-text.orna"),
    );
    assert!(!invalid_integer.is_ok());

}
