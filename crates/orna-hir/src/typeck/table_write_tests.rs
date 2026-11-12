use super::diagnostics_tests::{lower_source, spanned, with_code};
use super::table_write::{
    AUTOMATIC_KEY_REKEY, COMPUTED_INSERT, COMPUTED_UPDATE, KEY_IMMUTABLE, MISSING_FIELD,
    UNKNOWN_FIELD,
};
use super::typeck;
use crate::Program;

/// A table with a key, a required field, a defaulted field and a computed field.
const CONTACT: &str = "pub table Contact(id: Str) {\n    first: Str,\n    last: Str,\n    country: Str = \"GB\",\n    full_name: Str => \"{first} {last}\",\n}\n";

/// Lowers and types `CONTACT` followed by one function whose body is `body`.
fn contact_program(body: &str) -> (String, Program) {
    let source = format!("{CONTACT}pub fn run() = {body};\n");
    let mut program = lower_source(&source);
    typeck(&mut program);
    (source, program)
}

#[test]
fn an_insert_omitting_a_required_field_is_e2002_and_defaulted_fields_are_not_required() {
    let (_, program) = contact_program(r#"Contact.insert({ id: "a", first: "A" })"#);
    let found = with_code(&program, MISSING_FIELD);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].1.message.contains("`last`"),
        "{}",
        found[0].1.message
    );
}

#[test]
fn an_insert_supplying_every_required_field_is_clean() {
    let (_, program) = contact_program(r#"Contact.insert({ id: "a", first: "A", last: "B" })"#);
    assert_eq!(
        program.diagnostics().count(),
        0,
        "{:?}",
        program.diagnostics().collect::<Vec<_>>()
    );
}

#[test]
fn an_upsert_omitting_the_primary_key_is_e2002() {
    let (_, program) = contact_program(r#"Contact.upsert({ first: "A", last: "B" })"#);
    let found = with_code(&program, MISSING_FIELD);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].1.message.contains("`id`"),
        "{}",
        found[0].1.message
    );
}

#[test]
fn an_update_without_a_key_field_is_clean() {
    let (_, program) = contact_program(r#"Contact.update("a", { first: "X" })"#);
    assert_eq!(
        program.diagnostics().count(),
        0,
        "{:?}",
        program.diagnostics().collect::<Vec<_>>()
    );
}

#[test]
fn an_unknown_row_field_is_e2003_and_only_that_rule_fires() {
    let source_body = r#"Contact.insert({ id: "a", first: "A", last: "B", banana: 42 })"#;
    let (source, program) = contact_program(source_body);
    let found = with_code(&program, UNKNOWN_FIELD);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].1.message.contains("banana"),
        "{}",
        found[0].1.message
    );
    assert!(spanned(&source, found[0].1).contains("banana"));
    assert!(with_code(&program, MISSING_FIELD).is_empty());
}

#[test]
fn a_computed_field_in_an_insert_is_e3010() {
    let (source, program) =
        contact_program(r#"Contact.insert({ id: "a", first: "A", last: "B", full_name: "W" })"#);
    let found = with_code(&program, COMPUTED_INSERT);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].1.message.contains("full_name"),
        "{}",
        found[0].1.message
    );
    assert!(spanned(&source, found[0].1).contains("full_name"));
}

#[test]
fn a_computed_field_in_an_upsert_is_e3010() {
    let (_, program) =
        contact_program(r#"Contact.upsert({ id: "a", first: "A", last: "B", full_name: "W" })"#);
    let found = with_code(&program, COMPUTED_INSERT);
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn a_computed_field_in_an_update_is_e3011() {
    let (_, program) = contact_program(r#"Contact.update("a", { full_name: "W" })"#);
    let found = with_code(&program, COMPUTED_UPDATE);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].1.message.contains("full_name"),
        "{}",
        found[0].1.message
    );
    assert!(with_code(&program, COMPUTED_INSERT).is_empty());
}

#[test]
fn a_primary_key_in_an_update_is_e3002() {
    let (_, program) = contact_program(r#"Contact.update("a", { id: "b" })"#);
    let found = with_code(&program, KEY_IMMUTABLE);
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn rekey_of_an_automatic_key_table_is_e3013() {
    let source = "pub table Note {\n    text: Str,\n}\n\npub fn run() = Note.rekey(1, 2);\n";
    let mut program = lower_source(source);
    typeck(&mut program);
    let found = with_code(&program, AUTOMATIC_KEY_REKEY);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        spanned(source, found[0].1).contains("Note.rekey"),
        "{:?}",
        found[0].1.span
    );
}

#[test]
fn an_automatic_key_insert_without_rekey_is_clean() {
    let source =
        "pub table Note {\n    text: Str,\n}\n\npub fn run() = Note.insert({ text: \"x\" });\n";
    let mut program = lower_source(source);
    typeck(&mut program);
    assert_eq!(
        program.diagnostics().count(),
        0,
        "{:?}",
        program.diagnostics().collect::<Vec<_>>()
    );
}
