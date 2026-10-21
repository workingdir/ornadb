use orna_semantic_v1::{
    Analysis, DIAG_DUPLICATE, DIAG_FIELD_TYPE, DIAG_MISSING_FIELD, DIAG_TYPE, DIAG_UNKNOWN_FIELD,
    DIAG_UNRESOLVED, ModuleInput, Namespace, Type, analyze,
};

const DECLARATIONS: &str = include_str!("fixtures/enum-construction-declarations.orna");

fn analyze_with_declarations(source: &str) -> Analysis {
    analyze(&[ModuleInput::new(
        "main.orna",
        format!("{DECLARATIONS}\n{source}"),
    )])
}

fn analyze_importer(source: &str) -> Analysis {
    analyze(&[
        ModuleInput::new("types.orna", DECLARATIONS),
        ModuleInput::new("consumer.orna", source),
    ])
}

fn diagnostics(analysis: &Analysis) -> Vec<(&str, &str)> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
        .collect()
}

fn result_type(analysis: &Analysis, module: &str, function: &str) -> Type {
    let symbol = analysis
        .modules
        .get(&Namespace(vec![module.to_owned()]))
        .and_then(|module| module.symbols.get(function))
        .unwrap_or_else(|| panic!("missing {module}.{function}"));
    match &symbol.ty {
        Type::Function { result, .. } => result.as_ref().clone(),
        other => panic!("{function} is not a function: {other:?}"),
    }
}

// ORNA-ENUM-002, ORNA-ENUM-003, ORNA-CASE-003, ORNA-CASE-005, ORNA-ENUM-005:
// the section 6 example checks with constructed payload and payload-free
// values, guards, equality, and enum fields and keys in tables.
#[test]
fn constructed_variants_type_check_as_their_enum() {
    let source = include_str!("fixtures/enum-construction-valid.orna");
    let analysis = analyze_with_declarations(source);

    assert!(analysis.is_ok(), "{source}: {:?}", analysis.diagnostics);
}

// ORNA-ENUM-001, ORNA-ENUM-003, ORNA-SYNTAX-001: construction is checked
// against the closed variant set and the declared payload row. Field errors
// carry the published codes of tests/invalid-metadata.json.
#[test]
fn invalid_constructions_report_one_precise_diagnostic() {
    let cases: [(&str, &str, (&str, &str)); 10] = [
        (
            "unknown payload-free variant",
            include_str!("fixtures/enum-construction-unknown-variant.orna"),
            (DIAG_UNRESOLVED, "enum `Level` has no variant `medium`"),
        ),
        (
            "unknown payload variant",
            include_str!("fixtures/enum-construction-unknown-payload-variant.orna"),
            (
                DIAG_UNRESOLVED,
                "enum `SyncResult` has no variant `pending`",
            ),
        ),
        (
            "unknown payload field",
            include_str!("fixtures/enum-construction-unknown-field.orna"),
            (DIAG_UNKNOWN_FIELD, "unknown field `extra`"),
        ),
        (
            "missing payload field",
            include_str!("fixtures/enum-construction-missing-field.orna"),
            (DIAG_MISSING_FIELD, "missing required field `retryable`"),
        ),
        (
            "mistyped payload field",
            include_str!("fixtures/enum-construction-wrong-field-type.orna"),
            (DIAG_FIELD_TYPE, "expected Int, found Str"),
        ),
        (
            "duplicated payload field",
            include_str!("fixtures/enum-construction-duplicate-field.orna"),
            (DIAG_DUPLICATE, "duplicate payload field `count`"),
        ),
        (
            "payload variant without its fields",
            include_str!("fixtures/enum-construction-bare-payload-variant.orna"),
            (DIAG_MISSING_FIELD, "missing required field `count`"),
        ),
        (
            "payload-free variant given a field",
            include_str!("fixtures/enum-construction-payload-free-fields.orna"),
            (DIAG_UNKNOWN_FIELD, "unknown field `weight`"),
        ),
        (
            "unknown variant stored in a table",
            include_str!("fixtures/enum-construction-stored-unknown-variant.orna"),
            (DIAG_UNRESOLVED, "enum `Level` has no variant `medium`"),
        ),
        (
            "variant returned as another type",
            include_str!("fixtures/enum-construction-wrong-result-type.orna"),
            (DIAG_TYPE, "static types are incompatible"),
        ),
    ];
    for (case, source, expected) in cases {
        let analysis = analyze_with_declarations(source);
        assert_eq!(diagnostics(&analysis), [expected], "{case}: {source}");
    }
}

// ORNA-ENUM-002, ORNA-ENUM-003 and the lexical construction rule: an
// imported enum constructs through its imported name or a module alias, and
// the value carries the declared enum type.
#[test]
fn imported_enums_construct_through_names_and_module_aliases() {
    let source = include_str!("fixtures/enum-construction-imported-consumer.orna");
    let analysis = analyze_importer(source);

    assert!(analysis.is_ok(), "{source}: {:?}", analysis.diagnostics);
    assert_eq!(
        result_type(&analysis, "consumer", "qualified"),
        Type::Named("Level".into())
    );
    assert_eq!(
        result_type(&analysis, "consumer", "qualified_payload"),
        Type::Named("SyncResult".into())
    );
}

#[test]
fn imported_constructions_check_variants_and_payload_rows() {
    let source = include_str!("fixtures/enum-construction-imported-invalid.orna");
    let analysis = analyze_importer(source);

    assert_eq!(
        diagnostics(&analysis),
        [
            (DIAG_MISSING_FIELD, "missing required field `count`"),
            (
                DIAG_UNRESOLVED,
                "enum `shared.Level` has no variant `medium`"
            ),
        ],
        "{source}"
    );
}
