use orna_semantic_v1::{
    DIAG_TYPE, ModuleInput, RowUnitInput, admit_row_unit, analyze,
};

fn analysis() -> orna_semantic_v1::Analysis {
    analyze(&[ModuleInput::new(
        "library/main.orna",
        include_str!("fixtures/key-path-tail-schema.orna"),
    )])
}

fn admit(
    analysis: &orna_semantic_v1::Analysis,
    table: &str,
    key_path: &[&str],
) -> orna_semantic_v1::RowUnitAdmission {
    let table_path = format!("library/{table}");
    let key_path = key_path
        .iter()
        .map(|component| (*component).to_owned())
        .collect::<Vec<_>>();
    let logical_path = format!("{table_path}/{}", key_path.join("/"));
    let source = include_str!("fixtures/key-path-tail-row.orna");
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
fn table_reference_and_nested_tuple_keys_flatten_in_declared_order() {
    let analysis = analysis();
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);

    let child = admit(&analysis, "Child", &["north~2fsite", "42.orna"]);
    assert!(child.is_ok(), "{:#?}", child.diagnostics);
    assert_eq!(child.key, ["north/site", "42"]);

    let alias = admit(&analysis, "AliasedChild", &["north~2fsite", "42.orna"]);
    assert!(alias.is_ok(), "{:#?}", alias.diagnostics);
    assert_eq!(alias.key, ["north/site", "42"]);

    let nested = admit(
        &analysis,
        "NestedComposite",
        &["north~2fsite", "42", "true", "2026-09-30.orna"],
    );
    assert!(nested.is_ok(), "{:#?}", nested.diagnostics);
    assert_eq!(nested.key, ["north/site", "42", "true", "2026-09-30"]);

    let noncanonical_integer = admit(&analysis, "Child", &["north~2fsite", "042.orna"]);
    assert!(!noncanonical_integer.is_ok());
}

#[test]
fn float_and_range_are_rejected_as_keys_while_ranges_remain_stored_data() {
    let valid = analysis();
    assert!(valid.is_ok(), "{:#?}", valid.diagnostics);

    assert!(valid
        .modules
        .values()
        .any(|module| module.symbols.contains_key("RangeData")));

    let invalid = analyze(&[ModuleInput::new(
        "library/main.orna",
        include_str!("fixtures/key-path-tail-invalid.orna"),
    )]);
    assert!(!invalid.is_ok());
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "Float is not a valid primary-key type"
    }));
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "Range<T> is not a primary-key type in version 1.0"
    }));
}
