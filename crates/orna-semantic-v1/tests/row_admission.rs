use std::collections::BTreeMap;

use orna_semantic_v1::{
    admit_row_unit, analyze, Analysis, DIAG_ASSERTION_EFFECT, DIAG_BAD_PATH, DIAG_TYPE,
    DIAG_UNRESOLVED, DIAG_UNSUPPORTED, ModuleInput, RowUnitInput,
};

fn analysis_for(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("contacts/main.orna", source)])
}

fn candidate<'a>(
    logical_path: &'a str,
    table_path: &'a str,
    key_path: &'a [String],
    source: &'a str,
) -> RowUnitInput<'a> {
    RowUnitInput {
        logical_path,
        table_path,
        key_path,
        source,
        source_bytes: source.as_bytes(),
        parse_as: "row_unit",
    }
}

fn assert_code(admission: &orna_semantic_v1::RowUnitAdmission, code: &str) {
    assert!(
        admission.diagnostics.iter().any(|diagnostic| diagnostic.code() == code),
        "expected {code}, got {:?}",
        admission.diagnostics
    );
}

fn assert_message(admission: &orna_semantic_v1::RowUnitAdmission, code: &str, message: &str) {
    assert!(
        admission
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == code && diagnostic.message() == message),
        "expected {code} / {message:?}, got {:?}",
        admission.diagnostics
    );
}

fn contact_analysis() -> Analysis {
    analysis_for(
        r#"
            pub table Contact(id: Str) {
                name: Str,
                emails: [Str],
            }
        "#,
    )
}

#[test]
fn valid_schema_backed_row_unit_is_admitted() {
    let analysis = contact_analysis();
    let key_path = ["alice-smith.orna".to_owned()];
    let row = candidate(
        "contacts/Contact/alice-smith.orna",
        "contacts/Contact",
        &key_path,
        r#"{ name: "Alice Smith", emails: ["alice@example.com"], }"#,
    );

    let admitted = admit_row_unit(&analysis, &row);

    assert!(admitted.is_ok(), "{:#?}", admitted.diagnostics);
    assert_eq!(admitted.owner, Some("contacts/Contact".to_owned()));
    assert_eq!(admitted.key, vec!["alice-smith"]);
    assert_eq!(
        admitted.fields,
        BTreeMap::from([
            (
                "emails".to_owned(),
                orna_semantic_v1::Type::List(Box::new(orna_semantic_v1::Type::Text))
            ),
            ("name".to_owned(), orna_semantic_v1::Type::Text),
        ])
    );
}

#[test]
fn row_unit_requires_the_published_parse_tag() {
    let analysis = contact_analysis();
    let key_path = ["alice-smith.orna".to_owned()];
    let mut row = candidate(
        "contacts/Contact/alice-smith.orna",
        "contacts/Contact",
        &key_path,
        r#"{ name: "Alice Smith", emails: ["alice@example.com"], }"#,
    );
    row.parse_as = "module_unit";

    let admitted = admit_row_unit(&analysis, &row);

    assert_code(&admitted, DIAG_UNSUPPORTED);
}

#[test]
fn row_unit_rejects_malformed_logical_and_table_paths() {
    let analysis = contact_analysis();
    let key_path = ["alice-smith.orna".to_owned()];
    let malformed_logical = candidate(
        "contacts/Contact/alice-smith",
        "contacts/Contact",
        &key_path,
        r#"{ name: "Alice Smith", emails: ["alice@example.com"], }"#,
    );
    let malformed_table = candidate(
        "contacts/Contact/alice-smith.orna",
        "contacts/Contact.orna",
        &key_path,
        r#"{ name: "Alice Smith", emails: ["alice@example.com"], }"#,
    );

    let logical_result = admit_row_unit(&analysis, &malformed_logical);
    let table_result = admit_row_unit(&analysis, &malformed_table);

    assert!(!logical_result.is_ok());
    assert_code(&logical_result, DIAG_BAD_PATH);
    assert!(!table_result.is_ok());
    assert_code(&table_result, DIAG_BAD_PATH);
}

#[test]
fn row_unit_rejects_invalid_utf8_source_bytes() {
    let analysis = contact_analysis();
    let key_path = ["alice-smith.orna".to_owned()];
    let source = r#"{ name: "Alice Smith", emails: ["alice@example.com"], }"#;
    let row = RowUnitInput {
        logical_path: "contacts/Contact/alice-smith.orna",
        table_path: "contacts/Contact",
        key_path: &key_path,
        source,
        source_bytes: &[0xff, 0xfe],
        parse_as: "row_unit",
    };

    let admitted = admit_row_unit(&analysis, &row);

    assert!(!admitted.is_ok());
    assert_code(&admitted, DIAG_BAD_PATH);
}

#[test]
fn row_unit_rejects_undeclared_and_unreachable_tables() {
    let analysis = contact_analysis();
    let key_path = ["alice-smith.orna".to_owned()];
    let undeclared = candidate(
        "contacts/Missing/alice-smith.orna",
        "contacts/Missing",
        &key_path,
        r#"{ name: "Alice Smith", emails: ["alice@example.com"], }"#,
    );
    let unreachable = candidate(
        "other/Contact/alice-smith.orna",
        "contacts/Contact",
        &key_path,
        r#"{ name: "Alice Smith", emails: ["alice@example.com"], }"#,
    );

    let undeclared_result = admit_row_unit(&analysis, &undeclared);
    let unreachable_result = admit_row_unit(&analysis, &unreachable);

    assert_code(&undeclared_result, DIAG_UNRESOLVED);
    assert!(!unreachable_result.is_ok());
    assert_code(&unreachable_result, DIAG_BAD_PATH);
}

fn reading_analysis() -> Analysis {
    analysis_for(
        r#"
            pub table Reading(sensor: Str, time: Int) {
                value: Decimal,
            }
        "#,
    )
}

#[test]
fn row_unit_rejects_malformed_key_escape() {
    let analysis = reading_analysis();
    let key_path = ["sensor".to_owned(), "~e.orna".to_owned()];
    let logical_path = format!("contacts/Reading/{}", key_path.join("/"));
    let row = candidate(
        &logical_path,
        "contacts/Reading",
        &key_path,
        "{ value: 1.5 }",
    );

    let admitted = admit_row_unit(&analysis, &row);

    assert!(!admitted.is_ok());
    assert_message(
        &admitted,
        DIAG_BAD_PATH,
        "project row key path is not canonically encoded",
    );
}

fn reading_candidate<'a>(
    logical_path: &'a str,
    key_path: &'a [String],
) -> RowUnitInput<'a> {
    candidate(
        logical_path,
        "contacts/Reading",
        key_path,
        "{ value: 1.5 }",
    )
}

#[test]
fn row_unit_rejects_missing_extra_ordered_and_mistyped_keys() {
    let analysis = reading_analysis();
    let cases = [
        vec!["sensor.orna".to_owned()],
        vec![
            "sensor".to_owned(),
            "42".to_owned(),
            "extra.orna".to_owned(),
        ],
        vec!["42".to_owned(), "sensor.orna".to_owned()],
        vec!["sensor".to_owned(), "not-an-int.orna".to_owned()],
    ];

    for key_path in cases {
        let logical_path = format!("contacts/Reading/{}", key_path.join("/"));
        let admitted = admit_row_unit(
            &analysis,
            &reading_candidate(&logical_path, &key_path),
        );
        assert!(!admitted.is_ok(), "unexpected admission for {key_path:?}");
        assert_code(&admitted, DIAG_TYPE);
    }
}


#[test]
fn row_unit_rejects_wrong_unknown_and_missing_non_key_fields() {
    let analysis = contact_analysis();
    let key_path = ["alice-smith.orna".to_owned()];
    let cases = [
        r#"{ name: 42, emails: ["alice@example.com"], }"#,
        r#"{ name: "Alice Smith", emails: ["alice@example.com"], unknown: true, }"#,
        r#"{ name: "Alice Smith" }"#,
    ];

    for source in cases {
        let row = candidate(
            "contacts/Contact/alice-smith.orna",
            "contacts/Contact",
            &key_path,
            source,
        );
        let admitted = admit_row_unit(&analysis, &row);
        assert!(!admitted.is_ok(), "unexpected admission for {source}");
        assert_code(&admitted, DIAG_TYPE);
    }
    let wrong = candidate(
        "contacts/Contact/alice-smith.orna",
        "contacts/Contact",
        &key_path,
        cases[0],
    );
    assert_message(
        &admit_row_unit(&analysis, &wrong),
        DIAG_TYPE,
        "table write field has an incompatible type: expected Str, found Int",
    );
    let unknown = candidate(
        "contacts/Contact/alice-smith.orna",
        "contacts/Contact",
        &key_path,
        cases[1],
    );
    assert_message(
        &admit_row_unit(&analysis, &unknown),
        DIAG_TYPE,
        "table write contains an unknown field `unknown`",
    );
    let missing = candidate(
        "contacts/Contact/alice-smith.orna",
        "contacts/Contact",
        &key_path,
        cases[2],
    );
    assert_message(
        &admit_row_unit(&analysis, &missing),
        DIAG_TYPE,
        "project row omits a required stored field",
    );
}

#[test]
fn row_unit_rejects_computed_field_supply() {
    let analysis = analysis_for(
        r#"
            pub table Contact(id: Str) {
                name: Str,
                full_name: Str => name,
            }
        "#,
    );
    let key_path = ["alice-smith.orna".to_owned()];
    let row = candidate(
        "contacts/Contact/alice-smith.orna",
        "contacts/Contact",
        &key_path,
        r#"{ name: "Alice Smith", full_name: "forged", }"#,
    );

    let admitted = admit_row_unit(&analysis, &row);

    assert!(!admitted.is_ok());
    assert_message(
        &admitted,
        DIAG_TYPE,
        "project row body cannot supply a computed field",
    );
}

#[test]
fn row_unit_preserves_table_assertion_rejection() {
    let analysis = analysis_for(
        r#"
            pub table User(id: Str) {
                name: Str,
                assert std.io.fs.read_text("private-input") == "ok";
            }
        "#,
    );
    let key_path = ["alice.orna".to_owned()];
    let row = candidate(
        "contacts/User/alice.orna",
        "contacts/User",
        &key_path,
        r#"{ name: "Alice" }"#,
    );

    let admitted = admit_row_unit(&analysis, &row);

    assert!(!admitted.is_ok());
    assert_message(
        &admitted,
        DIAG_ASSERTION_EFFECT,
        "declaration assertion uses forbidden filesystem effect",
    );
}
